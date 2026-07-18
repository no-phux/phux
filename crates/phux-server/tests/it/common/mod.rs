//! Shared scaffolding for the `phux-byc.6.*` wire integration tests.
//!
//! Each `tests/*.rs` file in this crate compiles to its own integration
//! binary. Cargo's convention for sharing helper code across binaries is
//! a `tests/common/mod.rs` that gets pulled in via `mod common;` from
//! each binary. The module name `common` is special-cased by Cargo:
//! files under `tests/common/` are NOT compiled as standalone test
//! binaries (avoiding the "unused dead code" warnings that would
//! otherwise appear in any binary that doesn't use a given helper).
//!
//! The helpers here intentionally avoid touching `phux-server`'s
//! internals — every interaction goes through the public `ServerRuntime`
//! API plus the wire-frame surface from `phux_protocol`. That keeps the
//! tests honest: a regression that only shows up over the wire will
//! show up here, even if `ServerState` unit tests keep passing.
//!
//! All `recv` paths in these helpers are wrapped in `tokio::time::timeout`
//! (`WIRE_RECV_TIMEOUT`). A hang is a failure, not a wait-for-Godot.

#![allow(dead_code, reason = "shared helpers; some binaries use a subset")]
#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]
#![allow(clippy::missing_panics_doc, reason = "tests")]
// `unreachable_pub` and `clippy::redundant_pub_crate` are mutually
// exclusive on this file: `pub(crate)` triggers the latter (module is
// private, so the restriction is "redundant"), while `pub` triggers
// the former (no re-export path). The cargo `tests/common/` pattern is
// non-negotiable — each integration binary `mod common;`s the file in
// fresh, so `pub` is the only visibility that actually exports helpers
// to the binaries that need them. Suppress `unreachable_pub` here and
// keep `pub`.
#![allow(unreachable_pub, reason = "tests/common shared-helpers pattern")]

pub mod builder;
pub mod screen;
pub mod tracing_capture;

use std::future::Future;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use bytes::BytesMut;
use phux_protocol::wire::frame::{AttachTarget, FrameKind, ViewportInfo};
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

/// Poll `UnixStream::connect(path)` until success or the deadline expires.
/// `path.exists()` is racy with the "stale-socket unlink + bind" sequence
/// the server performs at startup; only an actual connect is race-free.
pub async fn wait_for_socket(path: &Path, deadline: Duration) -> UnixStream {
    let start = Instant::now();
    let mut last_err: Option<std::io::Error> = None;
    while start.elapsed() < deadline {
        match UnixStream::connect(path).await {
            Ok(s) => return s,
            Err(e) => last_err = Some(e),
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

/// Build the canonical `ATTACH { ByName(name) }` used by the byc.6 tests.
/// 80x24 viewport, no scrollback requested — matches the byc.8 fixture so
/// the snapshot dimensions line up.
#[must_use]
pub fn attach_by_name(name: &str) -> FrameKind {
    FrameKind::Attach {
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
