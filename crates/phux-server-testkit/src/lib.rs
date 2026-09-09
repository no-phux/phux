//! Shared scaffolding for phux-server's wire integration tests.
//!
//! This was `crates/phux-server/tests/common/`, included with `mod common;`
//! from each `tests/*.rs`. Cargo compiles every one of those files as its
//! own crate, so the module was compiled from scratch once per test binary —
//! 46 times. Each phux-server test binary measured ~19s of CPU, and ~86% of
//! what it compiled was this shared code rather than the test. As a real
//! crate it is compiled once and linked as an rlib.
//!
//! The helpers intentionally avoid touching `phux-server`'s internals —
//! every interaction goes through the public `ServerRuntime` API plus the
//! wire-frame surface from `phux_protocol`. That is what made this
//! extractable, and it is what keeps the tests honest: a regression that
//! only shows up over the wire will show up here, even if `ServerState`
//! unit tests keep passing.
//!
//! All `recv` paths in these helpers are wrapped in `tokio::time::timeout`
//! (`WIRE_RECV_TIMEOUT`). A hang is a failure, not a wait-for-Godot.

// Test scaffolding: the assertion helpers panic by contract, and a caller
// that misuses a fixture should fail loudly rather than thread a Result.
#![allow(clippy::expect_used, reason = "test scaffolding")]
#![allow(clippy::unwrap_used, reason = "test scaffolding")]
#![allow(clippy::panic, reason = "test scaffolding")]
#![allow(clippy::missing_panics_doc, reason = "test scaffolding")]
// These fixtures were private `tests/common/` types, so the workspace's
// `missing_debug_implementations` never applied to them; as a real crate's
// public surface it does. Several wrap foreign guards that are not Debug
// (`tracing`'s `DefaultGuard`, PTY handles), and no test formats a fixture
// with `{:?}` — a hand-written Debug per fixture would be pure ceremony.
#![allow(missing_debug_implementations, reason = "test scaffolding")]
// Same cause: these are public-API style lints, and this code only became a
// public API by being extracted. The crate is `publish = false` with exactly
// one consumer (phux-server's tests), so a rustdoc-listing summary convention
// and `#[must_use]` on fixture accessors buy nothing here — and reflowing 13
// doc comments to satisfy them would bury the change this commit is actually
// making. The prose itself is unchanged from when it lived in tests/common/.
#![allow(clippy::too_long_first_doc_paragraph, reason = "test scaffolding")]
#![allow(clippy::must_use_candidate, reason = "test scaffolding")]

pub mod builder;
pub mod fault;
pub mod relay;
pub mod screen;
pub mod tracing_capture;

use std::future::Future;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use bytes::BytesMut;
use phux_protocol::PROTOCOL_VERSION;
use phux_protocol::caps::{ClientCapabilities, ColorSupport, LayerSet};
use phux_protocol::input::key::{KeyAction, KeyEvent, ModSet, PhysicalKey};
use phux_protocol::wire::frame::{
    AttachTarget, CommandResult, DetachReason, FrameKind, TYPE_COMMAND_RESULT, TYPE_DETACHED,
    TYPE_HELLO_OK, ViewportInfo,
};
use phux_server::{ServerConfig, ServerError, ServerRuntime};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio::time::{sleep, timeout};

/// Deadline applied to every wire `recv` in the integration tests. The
/// margin is generous because these tests drive real PTYs and assert on
/// rendered screen content; under full-parallel nextest many PTY-backed
/// tests contend for CPU, so a tight deadline turns scheduler latency into
/// spurious failures. A genuinely hung server still fails the run, just
/// later. The recv resolves in milliseconds on the happy path, so the
/// deadline only elapses on an actual fault.
pub const WIRE_RECV_TIMEOUT: Duration = Duration::from_secs(15);

/// Deadline for the per-test socket-connect bootstrap. The margin is
/// generous on purpose, mirroring [`WIRE_RECV_TIMEOUT`]'s philosophy: under
/// full-parallel `just e2e` the server's `bind() + LocalSet::run_until` ramp
/// contends with every other PTY-backed test for CPU, so a tight deadline
/// turns scheduler latency into a spurious "socket never became connectable"
/// panic. The happy path connects in milliseconds, so this ceiling only
/// elapses on an actual fault (a server that genuinely never bound).
pub const SOCKET_CONNECT_DEADLINE: Duration = Duration::from_secs(10);

/// Deadline for joining a `ServerRuntime` that has already been told to shut
/// down.
///
/// Same philosophy as [`WIRE_RECV_TIMEOUT`], and for the same reason
/// (phux-br1f). This is the single most-repeated deadline in the suite — one
/// per test, at teardown, after `shutdown_tx.send(())` — and it asserts
/// nothing but "the server stops". It used to be a hand-written
/// `Duration::from_secs(5)` at every site: fine on an idle laptop, and on a
/// saturated one a measurement of the scheduler rather than of the server.
/// The happy path joins in milliseconds, so this ceiling only elapses on a
/// runtime that genuinely will not stop — which still fails the run, 30s
/// later, with the same message.
pub const SERVER_JOIN_DEADLINE: Duration = Duration::from_secs(30);

/// Spawn a [`ServerRuntime`] on the current `LocalSet`, optionally pre-
/// seeding a session by name. Returns the shutdown sender and the join
/// handle so each test can drive a clean shutdown.
///
/// Per ADR-0014 the server runs on a `LocalSet` because per-pane
/// `TerminalActor`s own `!Send` `libghostty_vt::Terminal`s — callers MUST
/// invoke this from inside a `LocalSet::run_until` (see [`run_local`]).
pub fn spawn_server(
    socket_path: PathBuf,
    pre_seeded: Option<&str>,
) -> (oneshot::Sender<()>, JoinHandle<Result<(), ServerError>>) {
    let (tx, rx) = oneshot::channel::<()>();
    let cfg = ServerConfig {
        socket_path,
        pre_seeded_session: pre_seeded.map(str::to_owned),
        seed_with_pty: false,
        seed_command: None,
        ..ServerConfig::with_default_socket()
    };
    let handle = tokio::task::spawn_local(async move {
        let server = ServerRuntime::new(cfg);
        server
            .run_async(async move {
                let _ = rx.await;
            })
            .await
    });
    (tx, handle)
}

/// Like [`spawn_server`] but lets the caller edit the [`ServerConfig`] before
/// the runtime binds it: a `[voice]` transcriber, a seed command, a policy.
pub fn spawn_server_with(
    socket_path: PathBuf,
    pre_seeded: Option<&str>,
    configure: impl FnOnce(&mut ServerConfig),
) -> (oneshot::Sender<()>, JoinHandle<Result<(), ServerError>>) {
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
        let server = ServerRuntime::new(cfg);
        server
            .run_async(async move {
                let _ = rx.await;
            })
            .await
    });
    (tx, handle)
}

/// Like [`spawn_server`] but pre-seeds a PTY-backed pane running `cmd`.
/// Used by the `input_dispatch` test to drive a deterministic echo
/// fixture (`cat`) for wire→PTY round-trip assertions.
pub fn spawn_server_with_seed_cmd(
    socket_path: PathBuf,
    pre_seeded: &str,
    cmd: portable_pty::CommandBuilder,
) -> (oneshot::Sender<()>, JoinHandle<Result<(), ServerError>>) {
    let (tx, rx) = oneshot::channel::<()>();
    let cfg = ServerConfig {
        socket_path,
        pre_seeded_session: Some(pre_seeded.to_owned()),
        seed_with_pty: true,
        seed_command: Some(cmd),
        ..ServerConfig::with_default_socket()
    };
    let handle = tokio::task::spawn_local(async move {
        let server = ServerRuntime::new(cfg);
        server
            .run_async(async move {
                let _ = rx.await;
            })
            .await
    });
    (tx, handle)
}

/// Like [`spawn_server_with_seed_cmd`] but also sets the server-wide
/// `defaults.term` (phux-ign). The runtime applies `ServerConfig::term`
/// over the seed command's baseline via `terminal_actor::apply_term`, so
/// setting `TERM` on `cmd` directly would be silently overwritten — this
/// helper is the honest way to spawn a seed pane under a specific `TERM`
/// (e.g. `ghostty` for the phux-0o8 kitty-keyboard round-trip harness).
pub fn spawn_server_with_seed_cmd_and_term(
    socket_path: PathBuf,
    pre_seeded: &str,
    cmd: portable_pty::CommandBuilder,
    term: &str,
) -> (oneshot::Sender<()>, JoinHandle<Result<(), ServerError>>) {
    let (tx, rx) = oneshot::channel::<()>();
    let cfg = ServerConfig {
        socket_path,
        pre_seeded_session: Some(pre_seeded.to_owned()),
        seed_with_pty: true,
        seed_command: Some(cmd),
        term: term.to_owned(),
        ..ServerConfig::with_default_socket()
    };
    let handle = tokio::task::spawn_local(async move {
        let server = ServerRuntime::new(cfg);
        server
            .run_async(async move {
                let _ = rx.await;
            })
            .await
    });
    (tx, handle)
}

/// Like [`spawn_server_with_seed_cmd`] but also sets the
/// `defaults.cwd-inheritance` policy. Used by the phux-nyx tests to
/// exercise the `session-root` and `last-cwd-per-window` modes against a
/// deterministic seed-pane fixture.
pub fn spawn_server_with_seed_cmd_and_cwd_mode(
    socket_path: PathBuf,
    pre_seeded: &str,
    cmd: portable_pty::CommandBuilder,
    cwd_inheritance: phux_config::CwdInheritance,
) -> (oneshot::Sender<()>, JoinHandle<Result<(), ServerError>>) {
    let (tx, rx) = oneshot::channel::<()>();
    let cfg = ServerConfig {
        socket_path,
        pre_seeded_session: Some(pre_seeded.to_owned()),
        seed_with_pty: true,
        seed_command: Some(cmd),
        cwd_inheritance,
        ..ServerConfig::with_default_socket()
    };
    let handle = tokio::task::spawn_local(async move {
        let server = ServerRuntime::new(cfg);
        server
            .run_async(async move {
                let _ = rx.await;
            })
            .await
    });
    (tx, handle)
}

/// Spawn a [`ServerRuntime`] that seeds panes with a *real PTY* but no
/// server-wide override command (`seed_with_pty = true`, `seed_command =
/// None`). Under this config a `CREATE_SESSION` carrying a non-empty wire
/// `command` runs that command in the seed pane — the override that wins in
/// [`spawn_server_with_seed_cmd`] is absent, so the wire command takes
/// effect. Used to verify the `CREATE_SESSION { command }` path end-to-end
/// against a deterministic PTY fixture (`phux-rhh`).
pub fn spawn_server_seed_pty_no_cmd(
    socket_path: PathBuf,
    pre_seeded: Option<&str>,
) -> (oneshot::Sender<()>, JoinHandle<Result<(), ServerError>>) {
    let (tx, rx) = oneshot::channel::<()>();
    let cfg = ServerConfig {
        socket_path,
        pre_seeded_session: pre_seeded.map(str::to_owned),
        seed_with_pty: true,
        seed_command: None,
        ..ServerConfig::with_default_socket()
    };
    let handle = tokio::task::spawn_local(async move {
        let server = ServerRuntime::new(cfg);
        server
            .run_async(async move {
                let _ = rx.await;
            })
            .await
    });
    (tx, handle)
}

/// Block on `fut` inside a fresh `current_thread` runtime + `LocalSet`.
/// Mirrors the byc.8 `attach_lifecycle` helper exactly so the wire
/// surface stays identical across tests.
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

/// Poll for a connection and complete the mandatory protocol-0.7 handshake.
///
/// Most wire integration tests exercise post-negotiation behavior. Tests that
/// target HELLO or pre-HELLO rejection must use [`wait_for_raw_socket`].
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

/// Non-panicking sibling of [`wait_for_socket`]: poll
/// `UnixStream::connect(path)` until success and return `Some(stream)`, or
/// return `None` once `deadline` elapses without a connect. Unlike
/// [`wait_for_socket`], a never-connectable socket is a *valid outcome* here,
/// not a panic — use this when the test is racing a server that may have
/// already reaped itself and torn the socket down (so "couldn't connect" is
/// indistinguishable from, and as acceptable as, "server gone"). The retry
/// cadence and connect semantics are identical to [`wait_for_socket`].
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

/// Read exactly one length-prefixed wire frame and return the full
/// framed bytes (4-byte BE header + body). Wrapped in
/// [`WIRE_RECV_TIMEOUT`]; panics on either timeout or I/O error so the
/// test fails loudly.
pub async fn recv_framed(stream: &mut UnixStream) -> Vec<u8> {
    timeout(WIRE_RECV_TIMEOUT, async {
        let mut header = [0u8; 4];
        stream.read_exact(&mut header).await.unwrap();
        let body_len = u32::from_be_bytes(header) as usize;
        let mut body = vec![0u8; body_len];
        stream.read_exact(&mut body).await.unwrap();
        let mut framed = Vec::with_capacity(4 + body_len);
        framed.extend_from_slice(&header);
        framed.extend_from_slice(&body);
        framed
    })
    .await
    .expect("timed out waiting for frame")
}

/// Decode one wire frame and return both the type byte (for type-level
/// assertions that don't want to match the full enum) and the decoded
/// [`FrameKind`].
pub async fn recv_typed(stream: &mut UnixStream) -> (u8, FrameKind) {
    let framed = recv_framed(stream).await;
    let type_byte = framed[4];
    let (frame, rest) = FrameKind::decode(&framed).expect("decode frame");
    assert!(rest.is_empty(), "decoder did not consume entire frame");
    (type_byte, frame)
}
/// Drain queued attach/bootstrap traffic until the server acknowledges DETACH.
///
/// Progressive bootstrap frames can already be in flight when a client sends
/// DETACH; tests must not assume the acknowledgement is the next wire frame.
pub async fn recv_until_detached(stream: &mut UnixStream) -> FrameKind {
    loop {
        let (_, frame) = recv_typed(stream).await;
        if matches!(frame, FrameKind::Detached { .. }) {
            return frame;
        }
    }
}

/// Like [`recv_typed`] but returns `None` on a clean connection close
/// (`UnexpectedEof` on the length prefix) instead of panicking. Use this
/// in read loops that may outlive the server: with the tmux server-exit
/// model (phux-60s) the server drops every client connection when its
/// last session is reaped, so a graceful EOF mid-loop is expected, not a
/// failure. Still panics on the [`WIRE_RECV_TIMEOUT`] (a hung server is a
/// loud failure) and on a partial/garbled frame.
pub async fn try_recv_typed(stream: &mut UnixStream) -> Option<(u8, FrameKind)> {
    let framed = timeout(WIRE_RECV_TIMEOUT, async {
        let mut header = [0u8; 4];
        match stream.read_exact(&mut header).await {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return None,
            Err(e) => panic!("recv header io error: {e}"),
        }
        let body_len = u32::from_be_bytes(header) as usize;
        let mut body = vec![0u8; body_len];
        stream.read_exact(&mut body).await.unwrap();
        let mut framed = Vec::with_capacity(4 + body_len);
        framed.extend_from_slice(&header);
        framed.extend_from_slice(&body);
        Some(framed)
    })
    .await
    .expect("timed out waiting for frame")?;
    let type_byte = framed[4];
    let (frame, rest) = FrameKind::decode(&framed).expect("decode frame");
    assert!(rest.is_empty(), "decoder did not consume entire frame");
    Some((type_byte, frame))
}

/// Encode a [`FrameKind`] into a length-prefixed wire buffer.
pub fn encode_frame(frame: &FrameKind) -> BytesMut {
    let mut buf = BytesMut::new();
    frame.encode(&mut buf);
    buf
}

/// Read frames until the `COMMAND_RESULT` for `request_id` arrives, bounded by
/// [`WIRE_RECV_TIMEOUT`] overall. Unrelated frames in between are skipped.
///
/// Six test files carried their own copy, four byte-identical and two
/// differing only in the panic wording.
///
/// The unbounded sibling is [`recv_command_result`]: use this one when a
/// missing reply should fail the test rather than hang it.
pub async fn await_command_result(stream: &mut UnixStream, request_id: u32) -> CommandResult {
    let deadline = tokio::time::Instant::now() + WIRE_RECV_TIMEOUT;
    while tokio::time::Instant::now() < deadline {
        let remaining = deadline - tokio::time::Instant::now();
        let Ok((type_byte, frame)) = timeout(remaining, recv_typed(stream)).await else {
            break;
        };
        if type_byte != TYPE_COMMAND_RESULT {
            continue;
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
    panic!("no COMMAND_RESULT with request_id={request_id} within deadline");
}

/// Read frames until the `COMMAND_RESULT` for `request_id` arrives, skipping
/// anything else, with no overall deadline of its own.
///
/// Four test files carried a byte-identical copy. Prefer
/// [`await_command_result`] in new tests; this exists because these call sites
/// deliberately lean on the per-read timeout inside [`recv_typed`] instead of
/// bounding the whole wait.
pub async fn recv_command_result(stream: &mut UnixStream, request_id: u32) -> CommandResult {
    loop {
        let (_type_byte, frame) = recv_typed(stream).await;
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

/// Bind an ephemeral loopback port, read it back, and drop the listener.
///
/// Inherently racy — the port is free when returned, not reserved — which is
/// why it belongs in one place with the caveat written down rather than in
/// each transport test that wants a port.
#[must_use]
pub fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// [`encode_frame`] as an owned `Vec<u8>`, for the socket helpers that want
/// bytes rather than a `BytesMut`.
///
/// `wt_attach.rs` keeps a private copy on purpose: it documents that it takes
/// no testkit dependency at all, so that its handshake deadline is visibly its
/// own rather than borrowed. Do not "finish the job" by wiring it up here.
#[must_use]
pub fn encode_frame_vec(frame: &FrameKind) -> Vec<u8> {
    encode_frame(frame).to_vec()
}

/// Signal shutdown and assert the server task joined cleanly.
///
/// Pairs with every `spawn_server*` in this module, which hand back exactly
/// this `(Sender, JoinHandle)`. Forty-nine call sites across nineteen test
/// files each spelled this block out by hand before it was promoted here.
///
/// Deliberately does NOT assert the socket was unlinked: most of those call
/// sites had no socket path in scope, and the ones that care about unlinking
/// say so themselves. `end_to_end.rs` wraps this with that extra assertion.
///
/// Drop your own client stream before calling this — the call sites that need
/// it keep their `drop(stream)` because the variable is theirs, not ours.
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

/// Build a `KeyEvent` for an ASCII printable, matching what a real client
/// sends: the text and unshifted codepoint both carry the character, and no
/// modifiers are set or consumed.
///
/// Eleven test files each had a byte-identical private copy of this before it
/// was promoted here.
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

/// Build the canonical `ATTACH { ByName(name) }` used by the byc.6 tests.
/// 80x24 viewport, no scrollback requested — matches the byc.8 fixture so
/// the snapshot dimensions line up.
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
    }
}

/// Write a frame to the stream and flush. Convenience wrapper so each
/// test reads as a sequence of named protocol steps instead of a
/// `write_all` + `flush` pair.
pub async fn send_frame(stream: &mut UnixStream, frame: &FrameKind) {
    let buf = encode_frame(frame);
    stream.write_all(&buf).await.unwrap();
    stream.flush().await.unwrap();
}

/// Read with [`WIRE_RECV_TIMEOUT`] but expect EOF: returns `Ok(())` if
/// the next `read` yields `0` bytes (clean half-close), `Err` otherwise.
/// Used by the detach test to assert that the server has fully torn the
/// connection down once the client closes its write side.
pub async fn expect_eof_within(stream: &mut UnixStream, deadline: Duration) -> Result<(), String> {
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

/// Assert the SPEC §14 fatal-close shape on a UDS connection: the peer that
/// broke the protocol is sent `DETACHED { reason: PROTOCOL_ERROR }`, and the
/// server then closes the transport.
///
/// The `ERROR` frame that precedes it stays with the caller — its code and
/// message name the *specific* violation and differ per test. What every
/// fatal-close path owes identically is this tail, so it is asserted once
/// here rather than re-derived per test binary.
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

/// The frame half of [`expect_protocol_error_close`], for transports whose
/// close is not a byte-stream EOF — a WebSocket peer is closed with a Close
/// message, so that test supplies its own ending and shares this assertion.
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
