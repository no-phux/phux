//! What an `ATTACH` delivers about the pane (SPEC §13): a bootstrap that
//! round-trips the canonical grid, a PTY resized to the client's viewport,
//! and a session that survives its last shell exiting.

use phux_protocol::wire::frame::{
    AttachTarget, Command, CommandResult, CommandValue, FrameKind, StateScope,
};
use tempfile::TempDir;

use phux_server_testkit::screen::Screen;
use phux_server_testkit::{
    SOCKET_CONNECT_DEADLINE, WIRE_RECV_TIMEOUT, attach_by_name, command, recv_typed,
    recv_until_deadline, run_local, send_frame, spawn_server_connected, spawn_server_with_seed_cmd,
    wait_for_socket,
};

use super::common::{attach, attached, output_until, sh};

fn fresh(cols: u16, rows: u16) -> Screen {
    Screen::with_scrollback(cols, rows, 1000).unwrap()
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

        let mut once = fresh(cols, rows);
        once.write(&payload);
        let resynth_1 = once.synthesized().unwrap();
        assert_eq!(once.dims().unwrap(), (cols, rows));
        let mut twice = fresh(cols, rows);
        twice.write(&resynth_1);
        let resynth_2 = twice.synthesized().unwrap();
        assert_eq!(resynth_1, resynth_2, "round trip must be a fixed point");
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

/// ADR-0162: the last shell exiting is a shell exiting. Nothing respawns:
/// the attached client sees `RESOURCE_CLOSED` with the exit status, and the
/// persistent seeded session stays, empty.
#[test]
fn last_shell_eof_closes_the_terminal_and_keeps_the_session() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket = tmp.path().join("phux.sock");
        let release = tmp.path().join("release");
        // Exit only once released, after the attach handshake has landed.
        let seed = sh(&format!(
            "until [ -f '{}' ]; do sleep 0.01; done; exit 0",
            release.display(),
        ));
        let (_shutdown, _server) = spawn_server_with_seed_cmd(socket.clone(), "demo", seed);
        let mut stream = wait_for_socket(&socket, SOCKET_CONNECT_DEADLINE).await;
        send_frame(&mut stream, &attach_by_name("demo")).await;
        attached(&mut stream).await;

        std::fs::write(&release, b"go").unwrap();
        let end = tokio::time::Instant::now() + WIRE_RECV_TIMEOUT;
        let closed = recv_until_deadline(&mut stream, end, |_, frame| match frame {
            FrameKind::ResourceClosed {
                exit_status,
                reason,
                ..
            } => Some((exit_status, reason)),
            FrameKind::Detached { .. } => panic!("a persistent session keeps its client"),
            _ => None,
        })
        .await;
        assert_eq!(
            closed,
            Some((Some(0), phux_protocol::wire::frame::CloseReason::Exited)),
            "the exited shell closes with its status"
        );

        let get_state = Command::GetState {
            scope: StateScope::Server,
        };
        let CommandResult::OkWith(CommandValue::State(snapshot)) =
            command(&mut stream, 10, get_state).await
        else {
            panic!("GET_STATE failed");
        };
        assert!(
            snapshot.resources.is_empty(),
            "nothing was respawned: {snapshot:?}"
        );
        let demo = snapshot
            .sessions
            .iter()
            .find(|session| session.name == "demo")
            .expect("the persistent session stays");
        assert_eq!(demo.window_count, 0);
        assert!(demo.keep_empty);
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
                    command: Command::attach_terminal(pane.clone(), None),
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
                assert_eq!(resized.dims().unwrap(), (43, 38));
                assert_retained_history(&mut resized);
            }
        });
    }
}

async fn resource_bootstrap(stream: &mut tokio::net::UnixStream) -> Screen {
    let mut terminal = None;
    loop {
        match recv_typed(stream).await.1 {
            FrameKind::BootstrapBegin { cols, rows, .. } => terminal = Some(fresh(cols, rows)),
            FrameKind::BootstrapChunk { payload, .. } => {
                terminal
                    .as_mut()
                    .expect("BEGIN before CHUNK")
                    .write(&payload);
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

fn assert_retained_history(terminal: &mut Screen) {
    assert!(
        terminal.scrollback_rows().unwrap() >= 100,
        "older shell output was lost"
    );
    terminal.scroll_to_top();
    assert_viewport_holds(terminal, b"HISTORY_ROW_001");
    terminal.scroll_to_bottom();
    assert_viewport_holds(terminal, b"HISTORY_TAIL");
}

fn assert_viewport_holds(terminal: &Screen, marker: &[u8]) {
    let bytes = terminal.synthesized().unwrap();
    assert!(bytes.windows(marker.len()).any(|s| s == marker));
}
