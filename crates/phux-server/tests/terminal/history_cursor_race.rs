//! phux-rv52 / phux-ijuj — a stale native history cursor degrades one
//! replica; it never tears the attach down.
//!
//! # The regression this fences
//!
//! With a client attached, splitting a pane (`C-a c` / `C-a %`) killed the
//! whole client:
//!
//! ```text
//! protocol error: frame is not valid from a server in the attached phase:
//!   Error { code: InternalError,
//!           message: "native history request failed: invalid snapshot handle or token" }
//! ```
//!
//! The mechanism is a guaranteed race, not a rare one. A pane spawned
//! mid-attach is created at a hardcoded 80x24
//! (`runtime::commands::spawn_pane_with_pty_and_colors`) and bootstrapped
//! immediately, so the client learns about it, reflows its layout, and sends
//! `TERMINAL_RESIZE` for the new leaf. `TerminalActor::handle_resize` calls
//! `invalidate_all_native_cursors`, which *drains* `native_cursor_owners`.
//! The client's `HISTORY_REQUEST` — issued off `BOOTSTRAP_READY`, hence
//! strictly after the resize on one ordered stream — then quotes a cursor
//! with no surviving binding. Every split hits this window.
//!
//! # Cursor status versus generation fence
//!
//! `ERROR` (0xF0) is connection-scoped and therefore wrong for either race.
//! While the bootstrap generation is still live, `HISTORY_TOMBSTONE` (0x98)
//! is the L1 §4.5 response: it names and invalidates only the exact progressive
//! history lease.
//!
//! Once `BOOTSTRAP_TOMBSTONE` has retired the whole generation, L1 §4.6 is
//! stricter: no page, output, or cursor status carrying that BootstrapId may
//! follow. A stale HISTORY request racing behind that tombstone is therefore
//! consumed without another generation-bound response. The tests below pin
//! both cases and prove the connection still completes an ordinary command.

#![cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]
#![allow(
    clippy::doc_markdown,
    reason = "the narrative above uses bare wire-frame names (HISTORY_REQUEST, TERMINAL_RESIZE, …) the way the sibling integration tests do"
)]

use bytes::Bytes;
use phux_protocol::PROTOCOL_VERSION;
use phux_protocol::caps::{
    BootstrapCapabilities, BootstrapProfile, ClientCapabilities, EngineCodec, EngineFeatureSet,
};
use phux_protocol::ids::{BootstrapId, StreamId, TerminalId};
use phux_protocol::wire::frame::{
    AttachTarget, Command, CommandResult, CommandValue, FrameKind, HistoryTombstoneReason,
    SpawnResult, StateScope, TombstoneReason, ViewportInfo,
};
use phux_server::DEFAULT_GROUP_ID;
use portable_pty::CommandBuilder;
use tempfile::TempDir;
use tokio::net::UnixStream;
use tokio::time::timeout;

use phux_server_testkit::{
    SERVER_JOIN_DEADLINE, SOCKET_CONNECT_DEADLINE, recv_typed, run_local, send_frame,
    spawn_server_with_seed_cmd, wait_for_raw_socket,
};

/// The session every case in this file attaches to.
const SESSION: &str = "history-race";

/// Deliberately not 80x24: `TerminalActor::handle_resize` returns early on a
/// repeat of the settled geometry, and a pane spawned mid-attach is created
/// at exactly 80x24. A no-op resize would never reach
/// `invalidate_all_native_cursors`, so the test would pass vacuously.
const RESIZE_COLS: u16 = 132;
/// See [`RESIZE_COLS`].
const RESIZE_ROWS: u16 = 43;

/// Connect and complete HELLO negotiating the native checkpoint profile.
///
/// The testkit's `wait_for_socket` sends a HELLO advertising colour/layer
/// capabilities only, which negotiates `SynthesizedVtRaw` — a profile under
/// which `HISTORY_REQUEST` is answered `CodecFailure` before it ever reaches
/// the actor, so it cannot exercise the cursor race. This mirrors the same
/// native handshake `release_bootstrap_milestones.rs` performs.
async fn connect_native(path: &std::path::Path) -> UnixStream {
    let mut stream = wait_for_raw_socket(path, SOCKET_CONNECT_DEADLINE).await;
    send_frame(
        &mut stream,
        &FrameKind::Hello {
            client_name: "phux-history-cursor-race".to_owned(),
            protocol_major: PROTOCOL_VERSION.major,
            protocol_minor: PROTOCOL_VERSION.minor,
            protocol_patch: PROTOCOL_VERSION.patch,
            client_caps: ClientCapabilities::new().with_bootstrap(
                BootstrapCapabilities::new().with_native(
                    EngineCodec::LibghosttyCheckpointV2,
                    EngineFeatureSet::required_native(),
                ),
            ),
        },
    )
    .await;
    let (_, reply) = recv_typed(&mut stream).await;
    assert!(
        matches!(
            reply,
            FrameKind::HelloOk {
                selected_profile: BootstrapProfile::NativeState {
                    codec: EngineCodec::LibghosttyCheckpointV2,
                    ..
                },
                ..
            }
        ),
        "HISTORY_REQUEST requires the negotiated native checkpoint profile; got {reply:?}",
    );
    stream
}

/// Receive one frame, failing the test if it is a connection-scoped `ERROR`.
///
/// This is the assertion the regression is really about: under the old
/// behavior the server answered the stale cursor with `FrameKind::Error`,
/// which a real client treats as fatal in the attached phase. Routing every
/// read through here means no case in this file can pass while an `ERROR`
/// is on the wire, whatever else it observes.
async fn recv_no_error(stream: &mut UnixStream, context: &str) -> FrameKind {
    let (_, frame) = recv_typed(stream).await;
    if let FrameKind::Error {
        request_id,
        code,
        message,
    } = &frame
    {
        panic!(
            "{context}: server sent a connection-scoped ERROR \
             (request_id={request_id:?}, code={code:?}, message={message:?}); \
             a per-cursor history failure must degrade one replica with \
             HISTORY_TOMBSTONE, not end the attach (phux-rv52, phux-ijuj)",
        );
    }
    frame
}

/// Attach to [`SESSION`] and drain through `ATTACH_READY`.
///
/// Scrollback is requested so the attach exercises the same warm-history
/// path a real TUI client uses.
async fn attach(stream: &mut UnixStream, attach_id: u32) {
    send_frame(
        stream,
        &FrameKind::Attach {
            attach_id,
            target: AttachTarget::ByName(SESSION.to_owned()),
            viewport: ViewportInfo::new(80, 24),
            request_scrollback: true,
            scrollback_limit_lines: 50_000,
        },
    )
    .await;
    loop {
        if let FrameKind::AttachReady { attach_id: got } = recv_no_error(stream, "attach").await
            && got == attach_id
        {
            return;
        }
    }
}

/// Identity of the freshly-split pane's first bootstrap generation, plus the
/// history cursor its `BOOTSTRAP_READY` handed out.
#[derive(Debug)]
struct SpawnedGeneration {
    terminal_id: TerminalId,
    stream_id: StreamId,
    bootstrap_id: BootstrapId,
    cursor: Bytes,
}

/// Send `SPAWN_TERMINAL` and collect the new pane's `TERMINAL_SPAWNED` +
/// `BOOTSTRAP_BEGIN` + `BOOTSTRAP_READY`.
///
/// This is the `C-a c` half of the reproduction: an attached client splitting
/// a pane. Frames for the pre-existing seed pane (live output, its own
/// bootstrap) interleave freely, so everything is filtered on the new id.
async fn split_pane(stream: &mut UnixStream, request_id: u32) -> SpawnedGeneration {
    send_frame(
        stream,
        &FrameKind::SpawnTerminal {
            request_id,
            group: DEFAULT_GROUP_ID,
            // `cat` keeps the child alive for the whole race window; a
            // short-lived command could be reaped mid-resize.
            command: Some(vec!["/bin/cat".to_owned()]),
            cwd: None,
            env: None,
            term: None,
            satellite: None,
            owner_terminal: None,
            agent_session: None,
            initial_size: None,
        },
    )
    .await;

    let mut spawned: Option<TerminalId> = None;
    let mut begin: Option<(StreamId, BootstrapId)> = None;
    loop {
        match recv_no_error(stream, "split").await {
            FrameKind::TerminalSpawned {
                request_id: got,
                result,
            } if got == request_id => match result {
                SpawnResult::Ok(id) => spawned = Some(id),
                other => panic!("SPAWN_TERMINAL did not succeed: {other:?}"),
            },
            FrameKind::BootstrapBegin {
                terminal_id,
                stream_id,
                bootstrap_id,
                profile,
                ..
            } if Some(&terminal_id) == spawned.as_ref() => {
                assert!(
                    matches!(
                        profile,
                        phux_protocol::caps::BootstrapStreamProfile::NativeState {
                            codec: EngineCodec::LibghosttyCheckpointV2,
                        }
                    ),
                    "the split pane must bootstrap under the negotiated native \
                     profile, else no history cursor is ever leased; got {profile:?}",
                );
                begin = Some((stream_id, bootstrap_id));
            }
            FrameKind::BootstrapReady {
                terminal_id,
                stream_id,
                bootstrap_id,
                history_cursor,
            } if Some(&terminal_id) == spawned.as_ref() => {
                let (begin_stream, begin_bootstrap) =
                    begin.expect("BOOTSTRAP_READY preceded BOOTSTRAP_BEGIN for the split pane");
                assert_eq!(
                    (stream_id, bootstrap_id),
                    (begin_stream, begin_bootstrap),
                    "READY must close the generation BEGIN opened",
                );
                let cursor = history_cursor.expect(
                    "a native BOOTSTRAP_READY leases a history cursor; without one \
                     there is no stale-cursor race to fence",
                );
                return SpawnedGeneration {
                    terminal_id,
                    stream_id,
                    bootstrap_id,
                    cursor,
                };
            }
            _ => {}
        }
    }
}

/// Drain until the resize's `BOOTSTRAP_TOMBSTONE` for `generation` arrives.
///
/// This is the synchronisation point that makes the race deterministic: the
/// tombstone is published by `invalidate_all_native_cursors` itself, so
/// observing it proves the actor has already drained `native_cursor_owners`
/// — i.e. the cursor we are about to quote is genuinely stale, and the test
/// is not merely winning a coin flip against the actor's mailbox.
async fn await_resize_tombstone(stream: &mut UnixStream, generation: &SpawnedGeneration) {
    loop {
        if let FrameKind::BootstrapTombstone {
            terminal_id,
            stream_id,
            bootstrap_id,
            reason,
            ..
        } = recv_no_error(stream, "resize").await
            && terminal_id == generation.terminal_id
            && stream_id == generation.stream_id
            && bootstrap_id == generation.bootstrap_id
        {
            assert_eq!(
                reason,
                TombstoneReason::Resize,
                "the split pane's generation must be retired by the layout resize",
            );
            return;
        }
    }
}

/// Send `HISTORY_REQUEST` for `(terminal_id, stream_id, bootstrap_id, cursor)`
/// and return the `HISTORY_TOMBSTONE` reason the server answered with.
///
/// Fails if anything other than a tombstone for that exact identity comes
/// back — a `HISTORY_PAGE` would mean the lease was not actually invalidated,
/// and an `ERROR` is caught by [`recv_no_error`].
async fn request_history_expecting_tombstone(
    stream: &mut UnixStream,
    terminal_id: &TerminalId,
    stream_id: StreamId,
    bootstrap_id: BootstrapId,
    cursor: &Bytes,
) -> HistoryTombstoneReason {
    send_frame(
        stream,
        &FrameKind::HistoryRequest {
            terminal_id: terminal_id.clone(),
            stream_id,
            bootstrap_id,
            cursor: cursor.clone(),
            max_bytes: 1024 * 1024,
            max_rows: 512,
        },
    )
    .await;
    loop {
        match recv_no_error(stream, "history").await {
            FrameKind::HistoryTombstone {
                terminal_id: got_terminal,
                stream_id: got_stream,
                bootstrap_id: got_bootstrap,
                cursor: got_cursor,
                reason,
            } => {
                assert_eq!(
                    (&got_terminal, got_stream, got_bootstrap, &got_cursor),
                    (terminal_id, stream_id, bootstrap_id, cursor),
                    "HISTORY_TOMBSTONE must name the exact lease the client quoted, \
                     so the consumer can attribute it to one replica",
                );
                return reason;
            }
            FrameKind::HistoryPage { page_seq, .. } => panic!(
                "expected the invalidated lease to be tombstoned, got HISTORY_PAGE \
                 page_seq={page_seq}",
            ),
            FrameKind::HistoryRejected { reason, .. } => panic!(
                "expected HISTORY_TOMBSTONE, got the retryable HISTORY_REJECTED \
                 ({reason:?}) — the lease is gone, not merely under-budgeted",
            ),
            _ => {}
        }
    }
}

/// A request that reaches the server after its whole bootstrap generation was
/// retired gets no generation-bound response. The bootstrap tombstone already
/// invalidated the cursor; a later HISTORY_TOMBSTONE would itself target a
/// retired generation and violate L1 §4.6.
async fn assert_retired_history_is_silenced_and_connection_usable(
    stream: &mut UnixStream,
    generation: &SpawnedGeneration,
    request_id: u32,
) {
    send_frame(
        stream,
        &FrameKind::HistoryRequest {
            terminal_id: generation.terminal_id.clone(),
            stream_id: generation.stream_id,
            bootstrap_id: generation.bootstrap_id,
            cursor: generation.cursor.clone(),
            max_bytes: 1024 * 1024,
            max_rows: 512,
        },
    )
    .await;
    send_frame(
        stream,
        &FrameKind::Command {
            request_id,
            command: Command::GetState {
                scope: StateScope::Server,
            },
        },
    )
    .await;

    loop {
        match recv_no_error(stream, "post-retirement round trip").await {
            FrameKind::HistoryPage {
                terminal_id,
                stream_id,
                bootstrap_id,
                ..
            }
            | FrameKind::HistoryTombstone {
                terminal_id,
                stream_id,
                bootstrap_id,
                ..
            }
            | FrameKind::HistoryRejected {
                terminal_id,
                stream_id,
                bootstrap_id,
                ..
            } if terminal_id == generation.terminal_id
                && stream_id == generation.stream_id
                && bootstrap_id == generation.bootstrap_id =>
            {
                panic!(
                    "server emitted history data/status after BOOTSTRAP_TOMBSTONE retired \
                     ({stream_id:?}, {bootstrap_id:?})",
                );
            }
            FrameKind::CommandResult {
                request_id: got,
                result,
            } if got == request_id => match result {
                CommandResult::OkWith(CommandValue::State(snapshot)) => {
                    assert!(
                        !snapshot.panes.is_empty(),
                        "the attach must still see its panes after retirement",
                    );
                    return;
                }
                other => panic!("post-retirement GET_STATE failed: {other:?}"),
            },
            _ => {}
        }
    }
}

/// The connection is still usable: an ordinary request/reply completes.
///
/// This is the user-visible claim the whole fix is about. `GET_STATE` is a
/// cheap correlated round-trip; under the old behavior the client had already
/// been torn down by the `ERROR` and would never see this reply.
async fn assert_connection_still_usable(stream: &mut UnixStream, request_id: u32) {
    send_frame(
        stream,
        &FrameKind::Command {
            request_id,
            command: Command::GetState {
                scope: StateScope::Server,
            },
        },
    )
    .await;
    loop {
        if let FrameKind::CommandResult {
            request_id: got,
            result,
        } = recv_no_error(stream, "post-history round trip").await
            && got == request_id
        {
            match result {
                CommandResult::OkWith(CommandValue::State(snapshot)) => {
                    assert!(
                        !snapshot.panes.is_empty(),
                        "the attach must still see its panes after a tombstoned cursor",
                    );
                    return;
                }
                other => panic!("post-tombstone GET_STATE failed: {other:?}"),
            }
        }
    }
}

/// Seed a server whose one pane is a long-lived `cat`.
fn spawn_seeded_server(
    socket_path: std::path::PathBuf,
) -> (
    tokio::sync::oneshot::Sender<()>,
    tokio::task::JoinHandle<Result<(), phux_server::ServerError>>,
) {
    spawn_server_with_seed_cmd(socket_path, SESSION, CommandBuilder::new("/bin/cat"))
}

async fn shutdown(
    stream: UnixStream,
    shutdown_tx: tokio::sync::oneshot::Sender<()>,
    server_handle: tokio::task::JoinHandle<Result<(), phux_server::ServerError>>,
) {
    drop(stream);
    shutdown_tx.send(()).ok();
    timeout(SERVER_JOIN_DEADLINE, server_handle)
        .await
        .expect("server did not shut down after the shutdown signal")
        .expect("server join")
        .expect("server run_async ok");
}

/// phux-rv52: attach, split, resize the split leaf, then quote the cursor the
/// retired generation's `BOOTSTRAP_READY` handed out. The bootstrap tombstone
/// is the terminal status for that generation: no later history response may
/// cross the writer fence, and the connection remains usable.
#[test]
fn stale_cursor_after_split_resize_is_silenced_after_generation_retirement() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");
        let (shutdown_tx, server_handle) = spawn_seeded_server(socket_path.clone());
        let mut stream = connect_native(&socket_path).await;

        attach(&mut stream, 1).await;
        let generation = split_pane(&mut stream, 7).await;

        // The client reflows its layout on TERMINAL_SPAWNED and sizes the new
        // leaf to the real split geometry — never the 80x24 the server picked.
        send_frame(
            &mut stream,
            &FrameKind::TerminalResize {
                terminal_id: generation.terminal_id.clone(),
                cols: RESIZE_COLS,
                rows: RESIZE_ROWS,
            },
        )
        .await;
        await_resize_tombstone(&mut stream, &generation).await;

        // The client's HISTORY_REQUEST was built from BOOTSTRAP_READY, so it
        // still quotes the pre-resize lease. BOOTSTRAP_TOMBSTONE already
        // retired that whole generation; the server must not follow it with a
        // cursor status carrying the retired id.
        assert_retired_history_is_silenced_and_connection_usable(&mut stream, &generation, 8).await;

        shutdown(stream, shutdown_tx, server_handle).await;
    });
}

/// phux-ijuj: the other `HISTORY_REQUEST` exit that used to be a
/// connection-scoped `ERROR`. A cursor naming a terminal this server has never
/// interned is `Released` — the lease died with the pane — not
/// `Error { TerminalNotFound }`.
#[test]
fn history_request_for_unknown_terminal_tombstones_as_released() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");
        let (shutdown_tx, server_handle) = spawn_seeded_server(socket_path.clone());
        let mut stream = connect_native(&socket_path).await;

        attach(&mut stream, 1).await;

        // Far above anything `intern_terminal_wire` could have allocated for a
        // one-pane server, so `terminal_from_wire` cannot resolve it.
        let ghost = TerminalId::local(9_999_999);
        let reason = request_history_expecting_tombstone(
            &mut stream,
            &ghost,
            StreamId::new(1).expect("1 is non-zero"),
            BootstrapId::new(1).expect("1 is non-zero"),
            &Bytes::from_static(b"cursor-for-a-pane-that-never-existed"),
        )
        .await;
        assert_eq!(
            reason,
            HistoryTombstoneReason::Released,
            "an unresolvable terminal means the lease was released with the pane",
        );

        assert_connection_still_usable(&mut stream, 9).await;

        shutdown(stream, shutdown_tx, server_handle).await;
    });
}
