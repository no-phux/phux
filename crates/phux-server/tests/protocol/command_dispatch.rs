//! Dispatch of the generic `COMMAND` envelope (SPEC §5, ADR-0021) and of the
//! L3 session create/rename keys, driven through the production read loop.

use std::time::Duration;

use base64::Engine as _;
use libghostty_vt::terminal::{Point, PointCoordinate};
use phux_core::screen::ScreenState;
use phux_protocol::PROTOCOL_VERSION;
use phux_protocol::caps::{ClientCapabilities, ColorSupport, LayerSet, ServerFeature};
use phux_protocol::ids::{InputOperationId, ResourceId};
use phux_protocol::input::InputEvent;
use phux_protocol::input::paste::{PasteEvent, PasteTrust};
use phux_protocol::wire::frame::{
    Command, CommandResult, CommandValue, ErrorCode, FrameKind, RESOURCE_AGENT_SESSION_KEY,
    SESSION_CREATE_KEY, SESSION_CREATE_RESULT_KEY, SESSION_CREATE_RESULT_KEY_PREFIX,
    SESSION_NAME_KEY, Scope, SpawnResult, StateScope,
};
use phux_protocol::wire::info::SessionSnapshot;
use portable_pty::CommandBuilder;
use tempfile::TempDir;
use tokio::net::UnixStream;
use tokio::time::timeout;

use phux_server_testkit::{
    SOCKET_CONNECT_DEADLINE, Spawn, WIRE_RECV_TIMEOUT, attach_by_name, command,
    join_after_shutdown, recv_typed, recv_until, recv_until_deadline, recv_until_detached,
    run_local, send_frame, spawn_resource, spawn_server_connected, spawn_server_seed_pty_no_cmd,
    spawn_server_with_seed_cmd, try_recv_typed, wait_for_raw_socket, wait_for_server_screen_text,
    wait_for_socket,
};

async fn attach(stream: &mut UnixStream, session: &str) -> SessionSnapshot {
    send_frame(stream, &attach_by_name(session)).await;
    recv_until(stream, |_, frame| match frame {
        FrameKind::Attached { snapshot, .. } => Some(snapshot),
        _ => None,
    })
    .await
}

async fn snapshot(stream: &mut UnixStream, request_id: u32) -> SessionSnapshot {
    let get_state = Command::GetState {
        scope: StateScope::Server,
    };
    match command(stream, request_id, get_state).await {
        CommandResult::OkWith(CommandValue::State(snapshot)) => snapshot,
        other => panic!("expected State, got {other:?}"),
    }
}

fn session_names(snapshot: &SessionSnapshot) -> Vec<&str> {
    snapshot.sessions.iter().map(|s| s.name.as_str()).collect()
}

const fn get_screen(
    terminal_id: ResourceId,
    scrollback: Option<u32>,
    cells: bool,
    format: u8,
) -> Command {
    Command::GetScreen {
        terminal_id,
        request_scrollback: scrollback,
        cells,
        format,
    }
}

async fn screen(stream: &mut UnixStream, request_id: u32, get: Command) -> ScreenState {
    match command(stream, request_id, get).await {
        CommandResult::OkWith(CommandValue::Json(json)) => {
            serde_json::from_str(&json).expect("GET_SCREEN reply is a ScreenState")
        }
        other => panic!("expected Json, got {other:?}"),
    }
}

async fn get_metadata(
    stream: &mut UnixStream,
    request_id: u32,
    scope: Scope,
    key: &str,
) -> Option<Vec<u8>> {
    send_frame(
        stream,
        &FrameKind::GetMetadata {
            request_id,
            scope,
            key: key.to_owned(),
        },
    )
    .await;
    recv_until(stream, |_, frame| match frame {
        FrameKind::MetadataValue {
            request_id: got,
            value,
        } if got == request_id => Some(value),
        _ => None,
    })
    .await
}

async fn set_global(stream: &mut UnixStream, request_id: u32, key: &str, value: Vec<u8>) {
    send_frame(
        stream,
        &FrameKind::SetMetadata {
            request_id,
            scope: Scope::Global,
            key: key.to_owned(),
            value,
        },
    )
    .await;
}

/// Write a `SESSION_CREATE_KEY` request and return its published result.
async fn create_session(
    stream: &mut UnixStream,
    request_id: u32,
    request: serde_json::Value,
) -> serde_json::Value {
    set_global(
        stream,
        request_id,
        SESSION_CREATE_KEY,
        serde_json::to_vec(&request).unwrap(),
    )
    .await;
    let bytes = get_metadata(
        stream,
        request_id + 1,
        Scope::Global,
        SESSION_CREATE_RESULT_KEY,
    )
    .await
    .expect("a successful create publishes its result");
    serde_json::from_slice(&bytes).unwrap()
}

fn created_terminal(result: &serde_json::Value) -> ResourceId {
    let id = result["terminal_id"]
        .as_u64()
        .expect("result carries a terminal_id");
    ResourceId::local(u32::try_from(id).unwrap())
}

fn pane_cwd(snapshot: &SessionSnapshot, id: &ResourceId) -> std::path::PathBuf {
    let pane = snapshot
        .resources
        .iter()
        .find(|p| &p.id == id)
        .expect("pane in snapshot");
    let raw = std::path::PathBuf::from(pane.cwd.as_ref().expect("pane has a cwd"));
    raw.canonicalize().unwrap_or(raw)
}

/// Read-only verbs on one server: the HELLO feature bits, `GET_STATE`,
/// `GET_SCREEN` (plain, cells, unknown format), `GET_TERMINAL_STATE`, the
/// `TerminalNotFound` path of every terminal verb, and concurrent correlated
/// replies across two connections.
#[test]
fn read_verbs_answer_over_the_wire() {
    run_local(async {
        let (server, mut stream) = spawn_server_connected(Some("work")).await;
        let pane = attach(&mut stream, "work").await.resources[0].clone();

        assert!(session_names(&snapshot(&mut stream, 1).await).contains(&"work"));

        let plain = screen(&mut stream, 2, get_screen(pane.id.clone(), None, false, 0)).await;
        assert_eq!(plain.schema_version, phux_core::screen::SCHEMA_VERSION);
        assert_eq!(Some(plain.pane), pane.id.local_id());
        assert_eq!((plain.cols, plain.rows), (pane.cols, pane.rows));
        assert_eq!(plain.lines.len(), usize::from(pane.rows));
        assert!(plain.scrollback.is_empty() && plain.cells.is_none() && plain.rendered.is_none());
        let with_cells = screen(&mut stream, 3, get_screen(pane.id.clone(), None, true, 0)).await;
        assert!(
            with_cells.cells.is_some(),
            "cells: true threads through dispatch"
        );
        let unknown_format =
            command(&mut stream, 4, get_screen(pane.id.clone(), None, false, 3)).await;
        assert!(
            matches!(
                unknown_format,
                CommandResult::Error {
                    code: ErrorCode::Unsupported,
                    ..
                }
            ),
            "an undefined format is refused as unsupported, never guessed: {unknown_format:?}"
        );

        let state = Command::GetTerminalState {
            terminal_id: pane.id.clone(),
            include_scrollback: false,
            max_scrollback_lines: 0,
        };
        let CommandResult::OkWith(CommandValue::Json(json)) = command(&mut stream, 5, state).await
        else {
            panic!("GET_TERMINAL_STATE must answer Json");
        };
        let obj: serde_json::Value = serde_json::from_str(&json).unwrap();
        for field in [
            "cols",
            "rows",
            "cells",
            "cursor",
            "scrollback",
            "scrollback_count_total",
            "shell_state",
            "timestamp_secs",
            "seq",
        ] {
            assert!(obj.get(field).is_some(), "TerminalState lacks {field}");
        }

        let ghost = ResourceId::local(99_999);
        for (request_id, verb) in [
            (6, get_screen(ghost.clone(), None, false, 0)),
            (
                7,
                Command::KillResource {
                    terminal_id: ghost.clone(),
                    operation_id: None,
                },
            ),
            (
                8,
                Command::GetTerminalState {
                    terminal_id: ghost.clone(),
                    include_scrollback: false,
                    max_scrollback_lines: 0,
                },
            ),
        ] {
            let result = command(&mut stream, request_id, verb).await;
            let CommandResult::Error {
                code: ErrorCode::TerminalNotFound,
                message,
            } = result
            else {
                panic!("request {request_id}: expected TerminalNotFound, got {result:?}");
            };
            assert!(
                request_id != 8 || message.contains("no such terminal"),
                "{message}"
            );
        }

        // Two connections interleaving GET_SCREENs get every reply, correlated.
        let mut other = server.connect().await;
        let (a, b) = tokio::join!(
            async {
                for id in 100..110 {
                    screen(&mut stream, id, get_screen(pane.id.clone(), None, false, 0)).await;
                }
            },
            async {
                for id in 200..210 {
                    screen(&mut other, id, get_screen(pane.id.clone(), None, false, 0)).await;
                }
            }
        );
        let ((), ()) = (a, b);
    });
}

/// Every feature bit the wire suites rely on is advertised in `HELLO_OK`.
#[test]
fn hello_ok_advertises_the_command_features() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket = tmp.path().join("phux.sock");
        let (shutdown, handle) = phux_server_testkit::spawn_server(socket.clone(), None);
        let mut raw = wait_for_raw_socket(&socket, SOCKET_CONNECT_DEADLINE).await;
        send_frame(
            &mut raw,
            &FrameKind::Hello {
                client_name: "features".to_owned(),
                protocol_major: PROTOCOL_VERSION.major,
                protocol_minor: PROTOCOL_VERSION.minor,
                protocol_patch: PROTOCOL_VERSION.patch,
                client_caps: ClientCapabilities::new()
                    .with_color_support(ColorSupport::TrueColor)
                    .with_layers(LayerSet::all()),
            },
        )
        .await;
        let FrameKind::HelloOk { server_caps, .. } = recv_typed(&mut raw).await.1 else {
            panic!("expected HELLO_OK");
        };
        for feature in [
            ServerFeature::GetPerf,
            ServerFeature::Whoami,
            ServerFeature::SshOrigin,
            ServerFeature::ListDirectory,
            ServerFeature::ListDirectoryHost,
            ServerFeature::Transcribe,
            ServerFeature::OpenListener,
        ] {
            assert!(
                server_caps.features.contains(feature),
                "{feature:?} not advertised"
            );
        }
        drop(raw);
        join_after_shutdown(shutdown, handle).await;
    });
}

/// `APPLY_INPUT` acks only after the bytes reached the real PTY.
#[test]
fn apply_input_acks_after_real_pty_write_and_flush() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");
        let mut cmd = CommandBuilder::new("/bin/sh");
        cmd.args([
            "-c",
            "IFS= read -r line; printf 'APPLIED:%s\\n' \"$line\"; sleep 1",
        ]);
        let (_shutdown, _server) = spawn_server_with_seed_cmd(socket_path.clone(), "work", cmd);
        let mut stream = wait_for_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;
        let terminal_id = attach(&mut stream, "work").await.resources[0].id.clone();

        send_frame(
            &mut stream,
            &FrameKind::Command {
                request_id: 77,
                command: Command::ApplyInput {
                    operation_id: InputOperationId::new([0x77; 16]).unwrap(),
                    terminal_id,
                    events: vec![InputEvent::Paste(PasteEvent {
                        trust: PasteTrust::Trusted,
                        data: b"hello-ack\n".to_vec(),
                    })],
                },
            },
        )
        .await;
        let mut output = Vec::new();
        let mut acknowledged = false;
        let deadline = tokio::time::Instant::now() + WIRE_RECV_TIMEOUT;
        recv_until_deadline(&mut stream, deadline, |_, frame| {
            match frame {
                FrameKind::CommandResult {
                    request_id: 77,
                    result,
                } => {
                    assert_eq!(result, CommandResult::Ok);
                    acknowledged = true;
                }
                FrameKind::ResourceOutput { bytes, .. } => output.extend_from_slice(&bytes),
                _ => {}
            }
            (acknowledged && output.windows(17).any(|w| w == b"APPLIED:hello-ack")).then_some(())
        })
        .await
        .unwrap_or_else(|| panic!("acknowledged={acknowledged}; PTY output={output:?}"));
    });
}

/// `GET_SCREEN` formats render through the server's own libghostty
/// Formatter: html carries styling, vt replays into a fresh engine with the
/// same text and cell styling, and `request_scrollback` bounds the capture.
#[test]
fn get_screen_formats_render_html_and_replayable_vt_bounded_by_scrollback() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");
        // 40 lines push the first into scrollback; the sentinel is not a
        // substring of any later line.
        let mut cmd = CommandBuilder::new("/bin/sh");
        cmd.args([
            "-c",
            "echo SCROLLBACK-FIRST-LINE; \
             i=2; while [ $i -le 40 ]; do echo \"scrollback-line-$i\"; i=$((i+1)); done; \
             printf '\\033[1;31mroundtrip-vt-check\\033[0m'; sleep 5",
        ]);
        let (_shutdown, _server) = spawn_server_with_seed_cmd(socket_path.clone(), "work", cmd);
        let mut stream = wait_for_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;
        let pane = attach(&mut stream, "work").await.resources[0].clone();
        wait_for_server_screen_text(
            &mut stream,
            &pane.id,
            "roundtrip-vt-check",
            WIRE_RECV_TIMEOUT,
        )
        .await;

        let html = screen(&mut stream, 23, get_screen(pane.id.clone(), None, false, 1)).await;
        let html = html.rendered.expect("format html populates rendered");
        assert_eq!(html.format, "html");
        assert!(html.data.contains("roundtrip-vt-check"), "{}", html.data);
        assert!(
            html.data.to_ascii_lowercase().contains("style") || html.data.contains("color"),
            "bold red text carries inline styling: {}",
            html.data
        );
        assert!(
            !html.data.contains("SCROLLBACK-FIRST-LINE"),
            "no scrollback requested"
        );
        let with_history = screen(
            &mut stream,
            26,
            get_screen(pane.id.clone(), Some(0), false, 1),
        )
        .await;
        assert!(
            with_history
                .rendered
                .expect("html")
                .data
                .contains("SCROLLBACK-FIRST-LINE"),
            "Some(0) reaches the oldest retained row"
        );

        let vt = screen(&mut stream, 24, get_screen(pane.id.clone(), None, true, 2)).await;
        let rendered = vt.rendered.clone().expect("format vt populates rendered");
        assert_eq!(rendered.format, "vt");
        assert!(
            vt.lines.is_empty() && vt.scrollback.is_empty(),
            "no duplicate text projection"
        );
        let styled = vt
            .cells
            .as_ref()
            .and_then(|cells| cells.iter().find(|c| c.style.bold))
            .expect("a bold source cell");
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(&rendered.data)
            .unwrap();
        let mut replay = libghostty_vt::Terminal::new(pane.cols, pane.rows).unwrap();
        replay.vt_write(&bytes);
        let mut formatter = libghostty_vt::fmt::Formatter::new(
            &replay,
            libghostty_vt::fmt::FormatterOptions::new()
                .with_format(libghostty_vt::fmt::Format::Plain)
                .with_trim(true),
        )
        .unwrap();
        let text = String::from_utf8_lossy(&formatter.format_alloc(None).unwrap()).into_owned();
        assert!(text.contains("roundtrip-vt-check"), "{text:?}");
        let replay_styled = replay
            .grid_ref(Point::Viewport(PointCoordinate {
                x: styled.col,
                y: u32::from(styled.row),
            }))
            .and_then(|grid_ref| grid_ref.cell())
            .and_then(libghostty_vt::screen::Cell::has_styling)
            .unwrap_or(false);
        assert!(
            replay_styled,
            "cell ({}, {}) keeps its styling",
            styled.col, styled.row
        );
    });
}

/// `KILL_RESOURCE` and `KILL_RESOURCES` ack `Ok` and close every live pane
/// named; unknown ids in a `KILL_RESOURCES` are skipped (idempotent).
#[test]
fn kill_resource_and_kill_resources_ack_and_close() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");
        let (_shutdown, _server) = spawn_server_seed_pty_no_cmd(socket_path.clone(), Some("work"));
        let mut stream = wait_for_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;
        let pane_a = attach(&mut stream, "work").await.resources[0].id.clone();
        let SpawnResult::Ok(pane_b) = spawn_resource(&mut stream, 41, Spawn::default()).await
        else {
            panic!("spawn b");
        };
        let SpawnResult::Ok(pane_c) = spawn_resource(&mut stream, 42, Spawn::default()).await
        else {
            panic!("spawn c");
        };

        let kill_one = Command::KillResource {
            terminal_id: pane_b.clone(),
            operation_id: None,
        };
        let kill_many = Command::KillResources {
            ids: vec![pane_a.clone(), pane_c.clone(), ResourceId::local(999_999)],
            operation_id: None,
        };
        for (request_id, kill, panes) in [
            (43, kill_one, vec![pane_b]),
            (44, kill_many, vec![pane_a, pane_c]),
        ] {
            send_frame(
                &mut stream,
                &FrameKind::Command {
                    request_id,
                    command: kill,
                },
            )
            .await;
            // The ack and each RESOURCE_CLOSED arrive in any order; the
            // last kill self-exits the server and closes the connection.
            let (mut acked, mut closed) = (false, Vec::new());
            while !(acked && closed.len() == panes.len()) {
                let Ok(Some((_, frame))) =
                    timeout(Duration::from_secs(3), try_recv_typed(&mut stream)).await
                else {
                    break;
                };
                match frame {
                    FrameKind::CommandResult {
                        request_id: got,
                        result,
                    } if got == request_id => {
                        assert_eq!(result, CommandResult::Ok);
                        acked = true;
                    }
                    FrameKind::ResourceClosed { terminal_id, .. }
                        if panes.contains(&terminal_id) =>
                    {
                        closed.push(terminal_id);
                    }
                    _ => {}
                }
            }
            assert!(acked, "kill {request_id} acks Ok");
            assert_eq!(
                closed.len(),
                panes.len(),
                "kill {request_id} closes {panes:?}: {closed:?}"
            );
        }
    });
}

mod sessions;
