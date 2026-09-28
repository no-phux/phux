//! phux-rv52 / phux-ijuj: a stale native history cursor degrades one replica;
//! it never tears the attach down with a connection-scoped `ERROR`.
//!
//! Splitting a pane mid-attach used to kill the client: the new pane is
//! created at 80x24 and bootstrapped, the client resizes the leaf (which
//! drains every native cursor), then quotes the now-dead cursor from
//! `BOOTSTRAP_READY`. Once `BOOTSTRAP_TOMBSTONE` retires a generation, L1 §4.6
//! forbids any further history status for it; a cursor naming an unknown
//! terminal gets `HISTORY_TOMBSTONE { Released }` (L1 §4.5).

#![cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]

use bytes::Bytes;
use phux_protocol::PROTOCOL_VERSION;
use phux_protocol::caps::{
    BootstrapCapabilities, BootstrapProfile, BootstrapStreamProfile, ClientCapabilities,
    EngineCodec, EngineFeatureSet,
};
use phux_protocol::ids::{BootstrapId, ResourceId, StreamId};
use phux_protocol::wire::frame::{
    AttachTarget, Command, CommandResult, CommandValue, FrameKind, HistoryTombstoneReason,
    SpawnResult, StateScope, TombstoneReason, ViewportInfo,
};
use portable_pty::CommandBuilder;
use tempfile::TempDir;
use tokio::net::UnixStream;

use phux_server_testkit::{
    SOCKET_CONNECT_DEADLINE, Spawn, join_after_shutdown, recv_typed, run_local, send_frame,
    spawn_server_with_seed_cmd, wait_for_raw_socket,
};

const SESSION: &str = "history-race";

/// Not 80x24: a repeat of the settled geometry returns early and would never
/// drain the cursors, so the test would pass vacuously.
const RESIZE: (u16, u16) = (132, 43);

/// HELLO negotiating the native checkpoint profile; the testkit's default
/// HELLO gets `SynthesizedVtRaw`, where history never reaches the actor.
async fn connect_native(path: &std::path::Path) -> UnixStream {
    let mut stream = wait_for_raw_socket(path, SOCKET_CONNECT_DEADLINE).await;
    let caps = ClientCapabilities::new().with_bootstrap(BootstrapCapabilities::new().with_native(
        EngineCodec::LibghosttySnapshotV1,
        EngineFeatureSet::required_native(),
    ));
    let hello = FrameKind::Hello {
        client_name: "phux-history-cursor-race".to_owned(),
        protocol_major: PROTOCOL_VERSION.major,
        protocol_minor: PROTOCOL_VERSION.minor,
        protocol_patch: PROTOCOL_VERSION.patch,
        client_caps: caps,
    };
    send_frame(&mut stream, &hello).await;
    let (_, reply) = recv_typed(&mut stream).await;
    assert!(
        matches!(
            reply,
            FrameKind::HelloOk {
                selected_profile: BootstrapProfile::NativeState { .. },
                ..
            }
        ),
        "{reply:?}",
    );
    stream
}

/// Every read in this file goes through here, so no case can pass while an
/// `ERROR` (fatal to an attached client) is on the wire.
async fn recv_no_error(stream: &mut UnixStream) -> FrameKind {
    let (_, frame) = recv_typed(stream).await;
    assert!(
        !matches!(frame, FrameKind::Error { .. }),
        "a per-cursor history failure must not send a connection-scoped ERROR: {frame:?}",
    );
    frame
}

/// Read until `pick` returns `Some`, failing on any `ERROR`.
async fn recv_until_ok<T>(
    stream: &mut UnixStream,
    mut pick: impl FnMut(FrameKind) -> Option<T>,
) -> T {
    loop {
        if let Some(value) = pick(recv_no_error(stream).await) {
            return value;
        }
    }
}

struct Generation {
    terminal_id: ResourceId,
    stream_id: StreamId,
    bootstrap_id: BootstrapId,
    cursor: Bytes,
}

impl Generation {
    fn is(&self, terminal_id: &ResourceId, stream_id: StreamId, bootstrap_id: BootstrapId) -> bool {
        (terminal_id, stream_id, bootstrap_id)
            == (&self.terminal_id, self.stream_id, self.bootstrap_id)
    }

    fn history_request(&self) -> FrameKind {
        FrameKind::HistoryRequest {
            terminal_id: self.terminal_id.clone(),
            stream_id: self.stream_id,
            bootstrap_id: self.bootstrap_id,
            cursor: self.cursor.clone(),
            max_bytes: 1024 * 1024,
            max_rows: 512,
        }
    }
}

/// `SPAWN_RESOURCE` (the `C-a c` split) and collect the new pane's native
/// bootstrap generation and the history cursor its READY leased.
async fn split_pane(stream: &mut UnixStream, request_id: u32) -> Generation {
    send_frame(stream, &Spawn::command(&["/bin/cat"]).frame(request_id)).await;
    let mut spawned = None;
    let mut begin = None;
    recv_until_ok(stream, |frame| match frame {
        FrameKind::ResourceSpawned {
            request_id: got,
            result,
        } if got == request_id => {
            let SpawnResult::Ok(id) = result else {
                panic!("SPAWN_RESOURCE failed: {result:?}");
            };
            spawned = Some(id);
            None
        }
        FrameKind::BootstrapBegin {
            terminal_id,
            stream_id,
            bootstrap_id,
            profile,
            ..
        } if Some(&terminal_id) == spawned.as_ref() => {
            assert!(
                matches!(profile, BootstrapStreamProfile::NativeState { .. }),
                "{profile:?}"
            );
            begin = Some((stream_id, bootstrap_id));
            None
        }
        FrameKind::BootstrapReady {
            terminal_id,
            stream_id,
            bootstrap_id,
            history_cursor,
        } if Some(&terminal_id) == spawned.as_ref() => {
            assert_eq!(
                Some((stream_id, bootstrap_id)),
                begin,
                "READY closes BEGIN's generation"
            );
            Some(Generation {
                terminal_id,
                stream_id,
                bootstrap_id,
                cursor: history_cursor.expect("a native READY leases a history cursor"),
            })
        }
        _ => None,
    })
    .await
}

const fn get_state(request_id: u32) -> FrameKind {
    FrameKind::Command {
        request_id,
        command: Command::GetState {
            scope: StateScope::Server,
        },
    }
}

/// The connection still completes an ordinary round trip, and no history
/// status for `retired` crosses the writer fence on the way.
async fn assert_usable(stream: &mut UnixStream, request_id: u32, retired: Option<&Generation>) {
    send_frame(stream, &get_state(request_id)).await;
    recv_until_ok(stream, |frame| match frame {
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
        } if retired.is_some_and(|g| g.is(&terminal_id, stream_id, bootstrap_id)) => {
            panic!("history status after BOOTSTRAP_TOMBSTONE retired its generation")
        }
        FrameKind::CommandResult {
            request_id: got,
            result,
        } if got == request_id => {
            let CommandResult::OkWith(CommandValue::State(snapshot)) = result else {
                panic!("GET_STATE failed: {result:?}");
            };
            assert!(!snapshot.resources.is_empty());
            Some(())
        }
        _ => None,
    })
    .await;
}

#[test]
fn stale_history_cursors_degrade_one_replica_not_the_attach() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");
        let (shutdown, server) = spawn_server_with_seed_cmd(
            socket_path.clone(),
            SESSION,
            CommandBuilder::new("/bin/cat"),
        );
        let mut stream = connect_native(&socket_path).await;
        let attach = FrameKind::Attach {
            attach_id: 1,
            target: AttachTarget::ByName(SESSION.to_owned()),
            viewport: ViewportInfo::new(80, 24),
            request_scrollback: true,
            scrollback_limit_lines: 50_000,
            role_policy: None,
        };
        send_frame(&mut stream, &attach).await;
        recv_until_ok(&mut stream, |f| {
            matches!(f, FrameKind::AttachReady { attach_id: 1 }).then_some(())
        })
        .await;

        // phux-ijuj: a cursor for a terminal this server never interned.
        let ghost = Generation {
            terminal_id: ResourceId::local(9_999_999),
            stream_id: StreamId::new(1).unwrap(),
            bootstrap_id: BootstrapId::new(1).unwrap(),
            cursor: Bytes::from_static(b"cursor-for-a-pane-that-never-existed"),
        };
        send_frame(&mut stream, &ghost.history_request()).await;
        let reason = recv_until_ok(&mut stream, |frame| match frame {
            FrameKind::HistoryTombstone {
                terminal_id,
                stream_id,
                bootstrap_id,
                cursor,
                reason,
            } => {
                assert!(ghost.is(&terminal_id, stream_id, bootstrap_id) && cursor == ghost.cursor);
                Some(reason)
            }
            FrameKind::HistoryPage { .. } | FrameKind::HistoryRejected { .. } => {
                panic!("expected HISTORY_TOMBSTONE, got {frame:?}")
            }
            _ => None,
        })
        .await;
        assert_eq!(reason, HistoryTombstoneReason::Released);
        assert_usable(&mut stream, 9, None).await;

        // phux-rv52: split, resize the leaf, then quote the retired cursor.
        let generation = split_pane(&mut stream, 7).await;
        let resize = FrameKind::ResizeTerminal {
            terminal_id: generation.terminal_id.clone(),
            cols: RESIZE.0,
            rows: RESIZE.1,
        };
        send_frame(&mut stream, &resize).await;
        // Observing the tombstone proves the cursor is already drained.
        recv_until_ok(&mut stream, |frame| match frame {
            FrameKind::BootstrapTombstone {
                terminal_id,
                stream_id,
                bootstrap_id,
                reason,
                ..
            } if generation.is(&terminal_id, stream_id, bootstrap_id) => {
                assert_eq!(reason, TombstoneReason::Resize);
                Some(())
            }
            _ => None,
        })
        .await;
        send_frame(&mut stream, &generation.history_request()).await;
        assert_usable(&mut stream, 8, Some(&generation)).await;

        drop(stream);
        join_after_shutdown(shutdown, server).await;
    });
}
