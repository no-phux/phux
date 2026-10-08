//! What an `ATTACH` delivers about the pane (SPEC §13): a bootstrap that
//! round-trips the canonical grid, a PTY resized to the client's viewport,
//! and a pane that survives its last shell exiting.

use std::time::Duration;

use libghostty_vt::Terminal as GhosttyTerminal;
use phux_protocol::wire::frame::{
    AttachTarget, Command, CommandResult, CommandValue, FrameKind, ResourceLifecycle, StateScope,
};
use phux_server::grid::SnapshotSynthesizer;
use tempfile::TempDir;

use phux_server_testkit::{
    SOCKET_CONNECT_DEADLINE, WIRE_RECV_TIMEOUT, attach_by_name, command, recv_typed,
    recv_until_deadline, run_local, send_frame, spawn_server_connected, spawn_server_with_seed_cmd,
    wait_for_socket,
};

use super::common::{attach, attached, output_until, sh};

fn fresh(cols: u16, rows: u16) -> GhosttyTerminal<'static, 'static> {
    let mut terminal = GhosttyTerminal::new(cols, rows).unwrap();
    terminal.set_scrollback_max_lines(Some(1000)).unwrap();
    terminal
}

/// `ATTACHED` carries the session graph and a real client id, then one
/// BEGIN/CHUNK/READY bootstrap whose bytes reproduce the grid (ADR-0013):
/// re-synthesizing after one round trip is a fixed point.
#[test]
fn attach_returns_session_graph_and_round_trip_snapshot() {
    run_local(async {
        let (_server, mut stream) = spawn_server_connected(Some("default")).await;
        send_frame(&mut stream, &attach_by_name("default")).await;
        let FrameKind::Attached {
            snapshot,
            initial_client_id,
            ..
        } = recv_typed(&mut stream).await.1
        else {
            panic!("expected ATTACHED first");
        };
        assert_eq!(snapshot.sessions.len(), 1);
        assert_eq!(snapshot.sessions[0].name, "default");
        assert_eq!((snapshot.windows.len(), snapshot.resources.len()), (1, 1));
        assert!(initial_client_id.get() >= 1);

        let FrameKind::BootstrapBegin {
            cols,
            rows,
            stream_id,
            bootstrap_id,
            ..
        } = recv_typed(&mut stream).await.1
        else {
            panic!("expected BOOTSTRAP_BEGIN");
        };
        assert_eq!((cols, rows), (80, 24));
        let FrameKind::BootstrapChunk {
            stream_id: chunk_stream,
            bootstrap_id: chunk_bootstrap,
            payload,
            ..
        } = recv_typed(&mut stream).await.1
        else {
            panic!("expected BOOTSTRAP_CHUNK");
        };
        assert_eq!((chunk_stream, chunk_bootstrap), (stream_id, bootstrap_id));
        assert!(matches!(
            recv_typed(&mut stream).await.1,
            FrameKind::BootstrapReady { stream_id: s, bootstrap_id: b, history_cursor: None, .. }
                if s == stream_id && b == bootstrap_id
        ));
        assert!(payload.starts_with(b"\x1b[!p\x1b[2J\x1b[H"));

        let synth = SnapshotSynthesizer::new().unwrap();
        let mut once = fresh(cols, rows);
        once.vt_write(&payload);
        let resynth_1 = synth.synthesize(&once).unwrap();
        assert_eq!((resynth_1.cols, resynth_1.rows), (cols, rows));
        let mut twice = fresh(cols, rows);
        twice.vt_write(&resynth_1.bytes);
        let resynth_2 = synth.synthesize(&twice).unwrap();
        assert_eq!(
            resynth_1.bytes, resynth_2.bytes,
            "round trip must be a fixed point"
        );
    });
}

/// `ATTACH` propagates the client viewport to the seed PTY's kernel winsize
/// (phux-2lj: vim drew only 24 rows of a 40-row client).
#[test]
fn attach_resizes_seed_pty_to_client_viewport() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket = tmp.path().join("phux.sock");
        let (_shutdown, _server) = spawn_server_with_seed_cmd(
            socket.clone(),
            "default",
            sh("while :; do stty size; sleep 0.05; done"),
        );
        let mut stream = wait_for_socket(&socket, SOCKET_CONNECT_DEADLINE).await;
        send_frame(
            &mut stream,
            &attach(AttachTarget::ByName("default".to_owned()), 120, 40),
        )
        .await;
        attached(&mut stream).await;
        // `stty size` prints `rows cols`.
        output_until(&mut stream, b"40 120").await;
    });
}

/// phux-bnbd: the last shell exiting replaces the child in place; the
/// attached client sees neither `RESOURCE_CLOSED` nor `DETACHED`.
#[test]
fn last_shell_eof_keeps_the_terminal_live() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket = tmp.path().join("phux.sock");
        let release = tmp.path().join("release");
        let exited = tmp.path().join("exited");
        // Exit only once released, after the attach handshake has landed.
        let seed = sh(&format!(
            "until [ -f '{}' ]; do sleep 0.01; done; echo done > '{}'; exit 0",
            release.display(),
            exited.display()
        ));
        let (_shutdown, _server) = spawn_server_with_seed_cmd(socket.clone(), "demo", seed);
        let mut stream = wait_for_socket(&socket, SOCKET_CONNECT_DEADLINE).await;
        send_frame(&mut stream, &attach_by_name("demo")).await;
        attached(&mut stream).await;

        std::fs::write(&release, b"go").unwrap();
        let deadline = tokio::time::Instant::now() + WIRE_RECV_TIMEOUT;
        while !exited.exists() {
            assert!(tokio::time::Instant::now() < deadline, "seed never exited");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let end = tokio::time::Instant::now() + Duration::from_millis(250);
        recv_until_deadline(&mut stream, end, |_, frame| {
            assert!(
                !matches!(
                    frame,
                    FrameKind::Detached { .. } | FrameKind::ResourceClosed { .. }
                ),
                "last-shell exit must not close: {frame:?}"
            );
            None::<()>
        })
        .await;

        let get_state = Command::GetState {
            scope: StateScope::Server,
        };
        let CommandResult::OkWith(CommandValue::State(snapshot)) =
            command(&mut stream, 10, get_state).await
        else {
            panic!("GET_STATE failed");
        };
        assert!(
            snapshot
                .resources
                .iter()
                .any(|r| r.lifecycle == ResourceLifecycle::Running && r.exit.is_none()),
            "a live Terminal must remain: {snapshot:?}"
        );
    });
}

/// A mobile/browser opening one existing shell must receive the output that
/// predates its subscription, not just the last screen.
#[test]
fn resource_attach_replays_existing_scrollback_in_both_synthesized_profiles() {
    use phux_protocol::caps::{ClientCapabilities, OutputMode};
    use phux_server_testkit::wait_for_server_screen_text;

    for mode in [OutputMode::Raw, OutputMode::StateSync] {
        run_local(async {
            let tmp = TempDir::new().unwrap();
            let socket = tmp.path().join("phux.sock");
            let (_shutdown, _server) = spawn_server_with_seed_cmd(
                socket.clone(),
                "history",
                sh(
                    "i=1; while [ \"$i\" -le 160 ]; do printf 'HISTORY_ROW_%03d\\n' \"$i\"; i=$((i+1)); done; printf 'HISTORY_TAIL\\n'; exec cat",
                ),
            );
            let mut owner = wait_for_socket(&socket, SOCKET_CONNECT_DEADLINE).await;
            let CommandResult::OkWith(CommandValue::State(snapshot)) = command(
                &mut owner,
                1,
                Command::GetState {
                    scope: StateScope::Server,
                },
            )
            .await
            else {
                panic!("server topology unavailable")
            };
            let pane = snapshot.resources.first().unwrap().id.clone();
            let mut probe = wait_for_socket(&socket, SOCKET_CONNECT_DEADLINE).await;
            wait_for_server_screen_text(&mut probe, &pane, "HISTORY_TAIL", WIRE_RECV_TIMEOUT).await;
            let mut watcher = super::common::connect_with(
                &socket,
                ClientCapabilities::new().with_output_mode(mode),
            )
            .await;
            send_frame(
                &mut watcher,
                &FrameKind::Command {
                    request_id: 100,
                    command: Command::AttachResource {
                        terminal_id: pane.clone(),
                        role_policy: None,
                    },
                },
            )
            .await;
            let mut terminal = resource_bootstrap(&mut watcher).await;
            assert_retained_history(&mut terminal);
            if mode == OutputMode::Raw {
                send_frame(
                    &mut watcher,
                    &FrameKind::ResizeTerminal {
                        terminal_id: pane,
                        cols: 43,
                        rows: 38,
                        cell_px: None,
                    },
                )
                .await;
                let mut resized = resource_bootstrap(&mut watcher).await;
                assert_eq!((resized.cols().unwrap(), resized.rows().unwrap()), (43, 38));
                assert_retained_history(&mut resized);
            }
        });
    }
}

async fn resource_bootstrap(
    stream: &mut tokio::net::UnixStream,
) -> GhosttyTerminal<'static, 'static> {
    let mut terminal = None;
    loop {
        match recv_typed(stream).await.1 {
            FrameKind::BootstrapBegin { cols, rows, .. } => terminal = Some(fresh(cols, rows)),
            FrameKind::BootstrapChunk { payload, .. } => {
                terminal
                    .as_mut()
                    .expect("BEGIN before CHUNK")
                    .vt_write(&payload);
            }
            FrameKind::BootstrapReady { .. } => return terminal.expect("BEGIN before READY"),
            FrameKind::Error { message, .. }
            | FrameKind::CommandResult {
                result: CommandResult::Error { message, .. },
                ..
            } => panic!("{message}"),
            _ => {}
        }
    }
}

fn assert_retained_history(terminal: &mut GhosttyTerminal<'_, '_>) {
    use libghostty_vt::terminal::ScrollViewport;
    assert!(
        terminal.scrollback_rows().unwrap() >= 100,
        "older shell output was lost"
    );
    for (position, marker) in [
        (ScrollViewport::Top, b"HISTORY_ROW_001".as_slice()),
        (ScrollViewport::Bottom, b"HISTORY_TAIL".as_slice()),
    ] {
        terminal.scroll_viewport(position);
        let screen = SnapshotSynthesizer::new()
            .unwrap()
            .synthesize(terminal)
            .unwrap();
        assert!(screen.bytes.windows(marker.len()).any(|s| s == marker));
    }
}
