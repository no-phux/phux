//! Shared scaffolding for phux-server's wire integration tests. A separate
//! crate so it compiles once instead of once per test binary. Helpers drive
//! the server only through the public `ServerRuntime` API and the wire, and
//! every `recv` is bounded by [`WIRE_RECV_TIMEOUT`].

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "test scaffolding"
)]
#![allow(
    clippy::missing_panics_doc,
    missing_debug_implementations,
    clippy::too_long_first_doc_paragraph,
    clippy::must_use_candidate,
    reason = "test scaffolding with one consumer; fixtures wrap non-Debug guards"
)]

pub mod builder;
pub mod relay;
pub mod screen;
pub mod tracing_capture;

use std::future::Future;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use bytes::BytesMut;
use phux_protocol::PROTOCOL_VERSION;
use phux_protocol::caps::{ClientCapabilities, ColorSupport, LayerSet};
use phux_protocol::ids::{GroupId, ResourceId, SatelliteHost};
use phux_protocol::input::key::{KeyAction, KeyEvent, ModSet, PhysicalKey};
use phux_protocol::wire::frame::{
    AttachTarget, Command, CommandResult, CommandValue, DetachReason, FrameKind, SpawnResource,
    SpawnResult, TYPE_COMMAND_RESULT, TYPE_DETACHED, TYPE_HELLO_OK, ViewportInfo,
};
use phux_server::{ServerConfig, ServerError, ServerRuntime};
use tempfile::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio::time::{sleep, timeout};

/// Deadline for every wire `recv`. Generous because PTY-backed tests contend
/// for CPU; the happy path resolves in milliseconds, so only a real hang
/// reaches it.
pub const WIRE_RECV_TIMEOUT: Duration = Duration::from_secs(15);

/// Deadline for the per-test socket-connect bootstrap (same rationale).
pub const SOCKET_CONNECT_DEADLINE: Duration = Duration::from_secs(10);

/// Deadline for joining a `ServerRuntime` that was told to shut down.
pub const SERVER_JOIN_DEADLINE: Duration = Duration::from_secs(30);

pub type ServerHandles = (oneshot::Sender<()>, JoinHandle<Result<(), ServerError>>);

/// Spawn a [`ServerRuntime`] on the current `LocalSet` (ADR-0014: pane actors
/// are `!Send`, so call from inside [`run_local`]), optionally pre-seeding a
/// session without a PTY. Returns the shutdown sender and join handle.
pub fn spawn_server(socket_path: PathBuf, pre_seeded: Option<&str>) -> ServerHandles {
    spawn_server_with(socket_path, pre_seeded, |_| {})
}

/// Like [`spawn_server`] but lets the caller edit the [`ServerConfig`] first.
pub fn spawn_server_with(
    socket_path: PathBuf,
    pre_seeded: Option<&str>,
    configure: impl FnOnce(&mut ServerConfig),
) -> ServerHandles {
    let (tx, rx) = oneshot::channel::<()>();
    let mut cfg = ServerConfig {
        socket_path,
        pre_seeded_session: pre_seeded.map(str::to_owned),
        seed_with_pty: false,
        seed_command: None,
        ..ServerConfig::with_default_socket()
    };
    configure(&mut cfg);
    let handle = tokio::task::spawn_local(async move {
        ServerRuntime::new(cfg)
            .run_async(async move {
                let _ = rx.await;
            })
            .await
    });
    (tx, handle)
}

/// Pre-seed `pre_seeded` with a PTY-backed pane running `cmd`.
pub fn spawn_server_with_seed_cmd(
    socket_path: PathBuf,
    pre_seeded: &str,
    cmd: portable_pty::CommandBuilder,
) -> ServerHandles {
    spawn_server_with(socket_path, Some(pre_seeded), |cfg| seed_pty(cfg, cmd))
}

/// Configure `cfg` to seed panes with a real PTY running `cmd`.
pub fn seed_pty(cfg: &mut ServerConfig, cmd: portable_pty::CommandBuilder) {
    cfg.seed_with_pty = true;
    cfg.seed_command = Some(cmd);
}

/// Seed panes with a real PTY but no override command, so a wire
/// `CREATE_SESSION { command }` decides what runs.
pub fn spawn_server_seed_pty_no_cmd(
    socket_path: PathBuf,
    pre_seeded: Option<&str>,
) -> ServerHandles {
    spawn_server_with(socket_path, pre_seeded, |cfg| cfg.seed_with_pty = true)
}

/// Block on `fut` inside a fresh `current_thread` runtime + `LocalSet`.
pub fn run_local<F>(fut: F)
where
    F: Future<Output = ()>,
{
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    let local = tokio::task::LocalSet::new();
    local.block_on(&rt, fut);
}

/// Poll for a connection and complete the HELLO handshake. Tests of HELLO
/// itself use [`wait_for_raw_socket`].
pub async fn wait_for_socket(path: &Path, deadline: Duration) -> UnixStream {
    let mut stream = wait_for_raw_socket(path, deadline).await;
    send_frame(
        &mut stream,
        &FrameKind::Hello {
            client_name: "phux-server-integration-test".to_owned(),
            protocol_major: PROTOCOL_VERSION.major,
            protocol_minor: PROTOCOL_VERSION.minor,
            protocol_patch: PROTOCOL_VERSION.patch,
            client_caps: ClientCapabilities::new()
                .with_color_support(ColorSupport::TrueColor)
                .with_layers(LayerSet::all()),
        },
    )
    .await;
    let (type_byte, frame) = recv_typed(&mut stream).await;
    assert_eq!(type_byte, TYPE_HELLO_OK);
    assert!(matches!(frame, FrameKind::HelloOk { .. }));
    stream
}

/// Owning handles for [`spawn_server_connected`]; dropping it stops the server.
#[must_use = "dropping the shutdown sender stops the server"]
pub struct SpawnedServer {
    _tmp: TempDir,
    socket_path: PathBuf,
    _shutdown: oneshot::Sender<()>,
    _server: JoinHandle<Result<(), ServerError>>,
}

impl SpawnedServer {
    /// Open another HELLO'd client against this server.
    pub async fn connect(&self) -> UnixStream {
        wait_for_socket(&self.socket_path, SOCKET_CONNECT_DEADLINE).await
    }
}

/// Spawn a [`ServerRuntime`] (optionally pre-seeded) in a temp dir and return
/// a HELLO'd client. Extra clients use [`SpawnedServer::connect`].
#[must_use = "dropping the shutdown sender stops the server"]
pub async fn spawn_server_connected(pre_seeded: Option<&str>) -> (SpawnedServer, UnixStream) {
    let tmp = TempDir::new().unwrap();
    let socket_path = tmp.path().join("phux.sock");
    let (shutdown, server) = spawn_server(socket_path.clone(), pre_seeded);
    let stream = wait_for_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;
    (
        SpawnedServer {
            _tmp: tmp,
            socket_path,
            _shutdown: shutdown,
            _server: server,
        },
        stream,
    )
}

/// Poll `UnixStream::connect(path)` without sending protocol frames.
pub async fn wait_for_raw_socket(path: &Path, deadline: Duration) -> UnixStream {
    let start = Instant::now();
    let mut last_err: Option<std::io::Error> = None;
    while start.elapsed() < deadline {
        match UnixStream::connect(path).await {
            Ok(stream) => return stream,
            Err(error) => last_err = Some(error),
        }
        sleep(Duration::from_millis(5)).await;
    }
    panic!(
        "socket {} never became connectable: last_err={:?}",
        path.display(),
        last_err,
    );
}

/// Like [`wait_for_raw_socket`] but returns `None` at the deadline, for tests
/// racing a server that may already have exited.
pub async fn try_connect_socket(path: &Path, deadline: Duration) -> Option<UnixStream> {
    let start = Instant::now();
    while start.elapsed() < deadline {
        if let Ok(s) = UnixStream::connect(path).await {
            return Some(s);
        }
        sleep(Duration::from_millis(5)).await;
    }
    None
}

/// Read one length-prefixed frame (header + body), or `None` on a clean EOF
/// before the header. Panics on timeout, I/O error, or a truncated frame.
async fn read_framed(stream: &mut UnixStream) -> Option<Vec<u8>> {
    timeout(WIRE_RECV_TIMEOUT, async {
        let mut header = [0u8; 4];
        match stream.read_exact(&mut header).await {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return None,
            Err(e) => panic!("recv header io error: {e}"),
        }
        let body_len = u32::from_be_bytes(header) as usize;
        let mut framed = vec![0u8; 4 + body_len];
        framed[..4].copy_from_slice(&header);
        stream.read_exact(&mut framed[4..]).await.unwrap();
        Some(framed)
    })
    .await
    .expect("timed out waiting for frame")
}

fn decode_framed(framed: &[u8]) -> (u8, FrameKind) {
    let (frame, rest) = FrameKind::decode(framed).expect("decode frame");
    assert!(rest.is_empty(), "decoder did not consume entire frame");
    (framed[4], frame)
}

/// Read one length-prefixed frame (header + body); panics on EOF or timeout.
pub async fn recv_framed(stream: &mut UnixStream) -> Vec<u8> {
    read_framed(stream).await.expect("connection closed")
}

/// Decode one wire frame, returning its type byte and [`FrameKind`].
pub async fn recv_typed(stream: &mut UnixStream) -> (u8, FrameKind) {
    decode_framed(&recv_framed(stream).await)
}

/// Like [`recv_typed`] but returns `None` on a clean EOF at a frame boundary
/// (the server exits when its last session is reaped).
pub async fn try_recv_typed(stream: &mut UnixStream) -> Option<(u8, FrameKind)> {
    read_framed(stream).await.as_deref().map(decode_framed)
}

/// Drain frames until `pred` returns `Some`. Each read is bounded by
/// [`WIRE_RECV_TIMEOUT`]; for an overall deadline use [`recv_until_deadline`].
pub async fn recv_until<T>(
    stream: &mut UnixStream,
    mut pred: impl FnMut(u8, FrameKind) -> Option<T>,
) -> T {
    loop {
        let (type_byte, frame) = recv_typed(stream).await;
        if let Some(value) = pred(type_byte, frame) {
            return value;
        }
    }
}

/// Like [`recv_until`], but returns `None` once `deadline` passes.
pub async fn recv_until_deadline<T>(
    stream: &mut UnixStream,
    deadline: tokio::time::Instant,
    mut pred: impl FnMut(u8, FrameKind) -> Option<T>,
) -> Option<T> {
    while tokio::time::Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        let Ok((type_byte, frame)) = timeout(remaining, recv_typed(stream)).await else {
            break;
        };
        if let Some(value) = pred(type_byte, frame) {
            return Some(value);
        }
    }
    None
}

/// Drain in-flight traffic until the server acknowledges DETACH.
pub async fn recv_until_detached(stream: &mut UnixStream) -> FrameKind {
    recv_until(stream, |_, frame| {
        matches!(frame, FrameKind::Detached { .. }).then_some(frame)
    })
    .await
}

/// Poll `GET_SCREEN` until the server's own grid for `terminal_id` shows
/// `needle`. Use a connection with no subscription so output does not
/// interleave with the replies.
pub async fn wait_for_server_screen_text(
    stream: &mut UnixStream,
    terminal_id: &ResourceId,
    needle: &str,
    deadline: Duration,
) {
    let start = Instant::now();
    for request_id in 1.. {
        assert!(
            start.elapsed() < deadline,
            "pane never showed {needle:?} within {deadline:?}",
        );
        send_frame(
            stream,
            &FrameKind::Command {
                request_id,
                command: Command::GetScreen {
                    terminal_id: terminal_id.clone(),
                    request_scrollback: None,
                    cells: false,
                    format: 0,
                },
            },
        )
        .await;
        match await_command_result(stream, request_id).await {
            CommandResult::OkWith(CommandValue::Json(json)) if json.contains(needle) => return,
            CommandResult::OkWith(CommandValue::Json(_)) => {}
            other => panic!("GET_SCREEN failed: {other:?}"),
        }
        sleep(Duration::from_millis(20)).await;
    }
}

/// Encode a [`FrameKind`] into a length-prefixed wire buffer.
pub fn encode_frame(frame: &FrameKind) -> BytesMut {
    let mut buf = BytesMut::new();
    frame.encode(&mut buf);
    buf
}

/// Skip frames until the `COMMAND_RESULT` for `request_id`, bounded by
/// [`WIRE_RECV_TIMEOUT`] overall.
pub async fn await_command_result(stream: &mut UnixStream, request_id: u32) -> CommandResult {
    let deadline = tokio::time::Instant::now() + WIRE_RECV_TIMEOUT;
    recv_until_deadline(stream, deadline, |type_byte, frame| {
        if type_byte != TYPE_COMMAND_RESULT {
            return None;
        }
        match frame {
            FrameKind::CommandResult {
                request_id: got,
                result,
            } if got == request_id => Some(result),
            _ => None,
        }
    })
    .await
    .unwrap_or_else(|| panic!("no COMMAND_RESULT with request_id={request_id} within deadline"))
}

/// Send `COMMAND { request_id, command }` and [`await_command_result`] its reply.
pub async fn command(stream: &mut UnixStream, request_id: u32, command: Command) -> CommandResult {
    send_frame(
        stream,
        &FrameKind::Command {
            request_id,
            command,
        },
    )
    .await;
    await_command_result(stream, request_id).await
}

/// An ephemeral loopback port that was free a moment ago (inherently racy).
#[must_use]
pub fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// [`encode_frame`] as an owned `Vec<u8>`.
#[must_use]
pub fn encode_frame_vec(frame: &FrameKind) -> Vec<u8> {
    encode_frame(frame).to_vec()
}

/// Signal shutdown and assert the server task joined cleanly. Drop client
/// streams first.
pub async fn join_after_shutdown(
    shutdown: oneshot::Sender<()>,
    server: JoinHandle<Result<(), ServerError>>,
) {
    shutdown.send(()).ok();
    timeout(SERVER_JOIN_DEADLINE, server)
        .await
        .expect("server did not shut down after the shutdown signal")
        .expect("server join")
        .expect("server run_async ok");
}

/// An unmodified press `KeyEvent` for an ASCII printable.
#[must_use]
pub fn ascii_key(c: char, key: PhysicalKey) -> KeyEvent {
    KeyEvent {
        action: KeyAction::Press,
        key,
        mods: ModSet::empty(),
        consumed_mods: ModSet::empty(),
        composing: false,
        text: Some(c.to_string()),
        unshifted_codepoint: Some(c as u32),
    }
}

/// The optional fields of a `SPAWN_RESOURCE` in group 1. `Default` is a
/// plain default-shell Terminal spawn on the receiving server.
#[derive(Debug, Clone, Default)]
pub struct Spawn {
    pub command: Option<Vec<String>>,
    pub cwd: Option<String>,
    pub env: Option<Vec<(String, String)>>,
    pub term: Option<String>,
    pub satellite: Option<SatelliteHost>,
    pub owner_terminal: Option<ResourceId>,
    pub agent_session: Option<Vec<u8>>,
    pub initial_size: Option<(u16, u16)>,
    pub resource: Option<Box<SpawnResource>>,
}

impl Spawn {
    /// Spawn `argv` instead of the default shell.
    #[must_use]
    pub fn command(argv: &[&str]) -> Self {
        Self {
            command: Some(argv.iter().map(|arg| (*arg).to_owned()).collect()),
            ..Self::default()
        }
    }

    /// The wire frame.
    #[must_use]
    pub fn frame(self, request_id: u32) -> FrameKind {
        FrameKind::SpawnResource {
            request_id,
            group: GroupId::new(1),
            command: self.command,
            cwd: self.cwd,
            env: self.env,
            term: self.term,
            satellite: self.satellite,
            owner_terminal: self.owner_terminal,
            agent_session: self.agent_session,
            initial_size: self.initial_size,
            resource: self.resource,
        }
    }
}

/// Send `spawn` and return the correlated `RESOURCE_SPAWNED` result, skipping
/// other frames, bounded by [`WIRE_RECV_TIMEOUT`] overall.
pub async fn spawn_resource(stream: &mut UnixStream, request_id: u32, spawn: Spawn) -> SpawnResult {
    send_frame(stream, &spawn.frame(request_id)).await;
    let deadline = tokio::time::Instant::now() + WIRE_RECV_TIMEOUT;
    recv_until_deadline(stream, deadline, |_, frame| match frame {
        FrameKind::ResourceSpawned {
            request_id: got,
            result,
        } if got == request_id => Some(result),
        _ => None,
    })
    .await
    .unwrap_or_else(|| panic!("no RESOURCE_SPAWNED with request_id={request_id} within deadline"))
}

/// `ATTACH { ByName(name) }` at 80x24 with no scrollback.
#[must_use]
pub fn attach_by_name(name: &str) -> FrameKind {
    attach_by_name_with_id(name, 1)
}

/// Build an `ATTACH` with an explicit connection-unique correlation id.
#[must_use]
pub fn attach_by_name_with_id(name: &str, attach_id: u32) -> FrameKind {
    FrameKind::Attach {
        attach_id,
        target: AttachTarget::ByName(name.to_owned()),
        viewport: ViewportInfo::new(80, 24),
        request_scrollback: false,
        scrollback_limit_lines: 0,
        role_policy: None,
    }
}

/// Write a frame to the stream and flush.
pub async fn send_frame(stream: &mut UnixStream, frame: &FrameKind) {
    let buf = encode_frame(frame);
    stream.write_all(&buf).await.unwrap();
    stream.flush().await.unwrap();
}

/// `Ok(())` if the next read within `deadline` is a clean EOF.
async fn expect_eof_within(stream: &mut UnixStream, deadline: Duration) -> Result<(), String> {
    let mut buf = [0u8; 16];
    match timeout(deadline, stream.read(&mut buf)).await {
        Ok(Ok(0)) => Ok(()),
        Ok(Ok(n)) => Err(format!(
            "expected EOF, got {n} bytes (server still talking?)",
        )),
        Ok(Err(e)) => Err(format!("read error while expecting EOF: {e}")),
        Err(_) => Err(format!(
            "no EOF within {}ms; connection still open",
            deadline.as_millis(),
        )),
    }
}

/// Assert the SPEC §14 fatal-close tail: `DETACHED { PROTOCOL_ERROR }`, then
/// EOF. The preceding `ERROR` frame is the caller's to check.
pub async fn expect_protocol_error_close(stream: &mut UnixStream, deadline: Duration) {
    let (type_byte, detached) = recv_typed(stream).await;
    assert_eq!(
        type_byte, TYPE_DETACHED,
        "ERROR must be followed by DETACHED (got type 0x{type_byte:02x})",
    );
    assert_protocol_error_detach(&detached);
    expect_eof_within(stream, deadline)
        .await
        .expect("server must close the transport after ERROR + DETACHED");
}

/// The frame half of [`expect_protocol_error_close`], for non-UDS transports.
pub fn assert_protocol_error_detach(frame: &FrameKind) {
    assert!(
        matches!(
            frame,
            FrameKind::Detached {
                reason: Some(DetachReason::ProtocolError),
                ..
            }
        ),
        "a protocol violation must close with DETACHED {{ reason: PROTOCOL_ERROR }}, got {frame:?}",
    );
}
