use std::path::{Path, PathBuf};
use std::process::ExitCode;

use phux_client::attach::connection::Connection;
use phux_client::layout::{SplitDir, Workspace};
use phux_client::layout_ops::{LayoutMutation, LayoutOps};
use phux_protocol::ids::{GroupId, SatelliteHost, SessionId, TerminalId, WindowId};
use phux_protocol::wire::frame::{
    Command, CommandResult, FrameKind, SpawnError, SpawnResult, StateScope,
};
use phux_server::runtime::default_socket_path;

use crate::commands::agent::{AgentSessionRecord, persist_record};
use crate::commands::{
    SpawnSplit, cli_runtime, parse_selector, report_no_server, request_command, resolve_targets,
};

/// `phux spawn` — create a Terminal without attaching (`SPAWN_TERMINAL`,
/// SPEC L1 §3.1). Does not auto-start a server.
///
/// With explicit placement, the target Terminal addresses the exact owning
/// window and shared layout metadata inserts the new leaf beside it. Without
/// placement, the pane joins the server's most recently active session (the
/// legacy `GET_STATE` focus heuristic). With `--satellite NAME`
/// a federation hub routes the spawn over its link to that satellite
/// (phux-v45.6) and the returned Terminal is satellite-tagged: the
/// printed id is addressable through the hub by the satellite-capable
/// verbs. On a non-hub server (or for an unknown name) the spawn is
/// refused with the typed `UnsupportedSatelliteRoute`; an unreachable
/// satellite fails fast with `SatelliteUnreachable`.
///
/// Output hygiene matches the other one-shot verbs: with `--json` stdout
/// carries only `{"terminal_id": N, "satellite": "NAME" | null}`;
/// diagnostics go to stderr with a nonzero exit.
#[allow(clippy::too_many_arguments)]
pub(crate) fn run_spawn(
    satellite: Option<String>,
    target: Option<String>,
    split: SpawnSplit,
    ratio: f32,
    cwd: Option<String>,
    json: bool,
    socket: Option<PathBuf>,
    command: Vec<String>,
) -> ExitCode {
    let socket_path = socket.unwrap_or_else(default_socket_path);
    let request_id = 1u32;
    let frame = FrameKind::SpawnTerminal {
        request_id,
        // v0.1 servers expose the single default group (SPEC §3.1).
        group: GroupId::new(1),
        command: if command.is_empty() {
            None
        } else {
            Some(command)
        },
        cwd,
        env: None,
        term: None,
        satellite: satellite.map(SatelliteHost::new),
        owner_terminal: None,
        agent_session: None,
    };
    let result = match target {
        Some(target) => dispatch_spawn_placed(
            &socket_path,
            frame,
            request_id,
            "spawn",
            &target,
            split,
            ratio,
            None,
        ),
        None => dispatch_spawn(&socket_path, &frame, "spawn", None),
    };
    match result {
        Ok(SpawnResult::Ok(terminal_id)) => print_spawned(&terminal_id, json),
        Ok(SpawnResult::Err(err)) => {
            report_spawn_error(&err);
            ExitCode::FAILURE
        }
        Ok(other) => {
            eprintln!("phux: unexpected SPAWN_TERMINAL result: {other:?}");
            ExitCode::FAILURE
        }
        Err(code) => code,
    }
}

/// Send a `SPAWN_TERMINAL` frame and return the matching `TERMINAL_SPAWNED`
/// result. Shared by `phux spawn` and `phux launch` (phux-ark7) so both
/// ride the identical wire path — the server injects `PHUX_TERMINAL_ID`
/// into the spawned pane regardless of which verb requested it.
///
/// On a connect/transport failure this prints the `no server` diagnostic
/// (attributed to `verb`) and returns the failure [`ExitCode`] in `Err`, so
/// callers only handle the `SpawnResult` variants.
///
/// The correlation id is not a parameter: it is read out of `frame`'s own
/// `request_id` field, so the id sent and the id waited on cannot drift.
pub(crate) fn dispatch_spawn(
    socket_path: &Path,
    frame: &FrameKind,
    verb: &str,
    agent_session: Option<&AgentSessionRecord>,
) -> Result<SpawnResult, ExitCode> {
    let rt = cli_runtime()?;
    rt.block_on(dispatch_spawn_async(socket_path, frame, agent_session))
        .map_err(|err| report_no_server(&err, socket_path, verb))
}

/// Open a connection, send `frame`, and return the correlated spawn outcome.
///
/// The wait used to be a hand-rolled `loop { recv() }` that matched
/// `TERMINAL_SPAWNED` and dropped every other frame, which meant a peer
/// answering with a correlated `ERROR` — the way `relay.rs`'s own
/// `handle_inbound` says a satellite MAY answer a relayed spawn — wedged
/// `phux spawn --satellite` until the transport died. It now rides
/// [`Connection::request_spawn`], and a refusal is folded into the same
/// `SpawnResult::Err` shape a hub already produces for exactly this case, so
/// `report_spawn_error` renders it without a new code path.
async fn dispatch_spawn_async(
    socket_path: &Path,
    frame: &FrameKind,
    agent_session: Option<&AgentSessionRecord>,
) -> Result<SpawnResult, phux_client::attach::AttachError> {
    let mut conn = Connection::connect(socket_path).await?;
    let (answer, interleaved) = conn.request_spawn(frame).await?.into_parts();
    for message in phux_client::state::degradation_notices(&interleaved) {
        eprintln!("phux: warning: partial results — {message}");
    }
    let mut result = answer.unwrap_or_else(|refusal| {
        SpawnResult::Err(SpawnError::SpawnFailed(format!(
            "server refused the spawn: {refusal}"
        )))
    });
    if let (Some(record), SpawnResult::Ok(terminal)) = (agent_session, &result) {
        let request_id = match frame {
            FrameKind::SpawnTerminal { request_id, .. } => *request_id,
            _ => 1,
        };
        if let Err(err) =
            persist_record(&mut conn, terminal, record, request_id.wrapping_add(1)).await
        {
            let cleanup = conn
                .request(
                    request_id.wrapping_add(4),
                    Command::KillTerminal {
                        terminal_id: terminal.clone(),
                    },
                )
                .await;
            let cleanup_note = match cleanup {
                Ok(reply) => match reply.into_parts().0 {
                    CommandResult::Ok => "spawned terminal removed".to_owned(),
                    other => format!("cleanup returned {other:?}"),
                },
                Err(cleanup_err) => format!("cleanup failed: {cleanup_err}"),
            };
            result = SpawnResult::Err(SpawnError::SpawnFailed(format!(
                "agent session record could not be confirmed: {err}; {cleanup_note}"
            )));
        }
    }
    Ok(result)
}

/// Resolve an explicit local owner, spawn into its exact server window, then
/// insert the returned leaf through shared `LayoutOps`. If layout publication
/// fails after spawn, kill the known new Terminal before returning failure.
#[allow(
    clippy::too_many_arguments,
    reason = "shared spawn placement keeps the complete CLI operation explicit"
)]
pub(crate) fn dispatch_spawn_placed(
    socket_path: &Path,
    mut frame: FrameKind,
    request_id: u32,
    verb: &str,
    target_text: &str,
    split: SpawnSplit,
    ratio: f32,
    agent_session: Option<&AgentSessionRecord>,
) -> Result<SpawnResult, ExitCode> {
    let selector = parse_selector(Some(target_text))?;
    let rt = cli_runtime()?;
    rt.block_on(async {
        let snapshot = match request_command(
            socket_path,
            Command::GetState {
                scope: StateScope::Server,
            },
        )
        .await
        {
            Ok(CommandResult::OkWith(phux_protocol::wire::frame::CommandValue::State(s))) => s,
            Ok(other) => {
                eprintln!("phux: unexpected GET_STATE result: {other:?}");
                return Err(ExitCode::FAILURE);
            }
            Err(err) => return Err(report_no_server(&err, socket_path, verb)),
        };
        let candidates = resolve_targets(socket_path, &selector, &snapshot).await;
        let Some(owner) = crate::selector::pick_target_pane(&candidates, &snapshot.focused_pane)
        else {
            eprintln!("phux: no such target");
            return Err(ExitCode::FAILURE);
        };
        if !matches!(owner, TerminalId::Local { .. }) {
            eprintln!("phux: explicit spawn placement is local-only");
            return Err(ExitCode::FAILURE);
        }
        let Some((owner_window, session)) = ownership_for_terminal(&snapshot, &owner) else {
            eprintln!("phux: target has no local session ownership");
            return Err(ExitCode::FAILURE);
        };
        let FrameKind::SpawnTerminal { owner_terminal, .. } = &mut frame else {
            eprintln!("phux: internal spawn placement error");
            return Err(ExitCode::FAILURE);
        };
        *owner_terminal = Some(owner.clone());
        let spawned = dispatch_spawn_async(socket_path, &frame, agent_session)
            .await
            .map_err(|err| report_no_server(&err, socket_path, verb))?;
        let SpawnResult::Ok(new_pane) = &spawned else {
            return Ok(spawned);
        };

        // Field-tagged compatibility means an older server can legally ignore
        // owner_terminal. Verify authoritative registry ownership before
        // publishing layout, otherwise L3 could reference a pane that belongs
        // to another session/window.
        let ownership_error = match request_command(
            socket_path,
            Command::GetState {
                scope: StateScope::Server,
            },
        )
        .await
        {
            Ok(CommandResult::OkWith(phux_protocol::wire::frame::CommandValue::State(state))) => {
                let owner_after = ownership_for_terminal(&state, &owner);
                let spawned_after = ownership_for_terminal(&state, new_pane);
                (owner_after != Some((owner_window, session))
                    || spawned_after != Some((owner_window, session)))
                .then_some(
                    "server did not honor explicit spawn ownership (unsupported or ownership mismatch)"
                        .to_owned(),
                )
            }
            Ok(other) => Some(format!("unexpected ownership verification result: {other:?}")),
            Err(err) => Some(format!("ownership verification failed: {err}")),
        };
        if let Some(err) = ownership_error {
            rollback_spawned(socket_path, new_pane, verb, &err).await;
            return Err(ExitCode::FAILURE);
        }

        let placement_error = match Connection::connect(socket_path).await {
            Ok(mut layout_conn) => {
                let dir = match split {
                    SpawnSplit::Horizontal => SplitDir::Horizontal,
                    SpawnSplit::Vertical => SplitDir::Vertical,
                };
                let placement = LayoutMutation::SplitPreservingFocus {
                    target: owner.clone(),
                    new_pane: new_pane.clone(),
                    dir,
                    ratio,
                };
                LayoutOps::new(&mut layout_conn, session, request_id.wrapping_add(1))
                    .mutate_or_seed(Workspace::single(owner), placement)
                    .await
                    .err()
                    .map(|err| err.to_string())
            }
            Err(err) => Some(err.to_string()),
        };
        if let Some(err) = placement_error {
            rollback_spawned(socket_path, new_pane, verb, &err).await;
            return Err(ExitCode::FAILURE);
        }
        Ok(spawned)
    })
}

async fn rollback_spawned(socket_path: &Path, pane: &TerminalId, verb: &str, reason: &str) {
    let cleanup = request_command(
        socket_path,
        Command::KillTerminal {
            terminal_id: pane.clone(),
        },
    )
    .await;
    match cleanup {
        Ok(CommandResult::Ok) => {
            eprintln!("phux: {verb} placement failed; spawned pane was removed: {reason}");
        }
        Ok(other) => {
            eprintln!("phux: {verb} placement failed ({reason}); cleanup returned {other:?}");
        }
        Err(err) => {
            eprintln!("phux: {verb} placement failed ({reason}); cleanup failed: {err}");
        }
    }
}

fn ownership_for_terminal(
    snapshot: &phux_protocol::wire::info::SessionSnapshot,
    terminal: &TerminalId,
) -> Option<(WindowId, SessionId)> {
    let window = snapshot
        .panes
        .iter()
        .find(|pane| &pane.id == terminal)?
        .window_id;
    let session = snapshot
        .windows
        .iter()
        .find(|candidate| candidate.id == window)?
        .session_id;
    Some((window, session))
}

/// Print the freshly spawned Terminal id — human line or the stable JSON
/// document (`terminal_id` is the satellite-local id when `satellite` is
/// non-null; address it through the hub as `satellite`+`terminal_id`).
fn print_spawned(terminal_id: &TerminalId, json: bool) -> ExitCode {
    let (id, host) = match terminal_id {
        TerminalId::Local { id } => (*id, None),
        TerminalId::Satellite { host, id } => (*id, Some(host.as_str())),
    };
    if json {
        let payload = serde_json::json!({ "terminal_id": id, "satellite": host });
        match serde_json::to_string_pretty(&payload) {
            Ok(s) => {
                outln!("{s}");
                ExitCode::SUCCESS
            }
            Err(err) => {
                eprintln!("phux: failed to serialize spawn result as JSON: {err}");
                ExitCode::FAILURE
            }
        }
    } else {
        match host {
            Some(host) => outln!("spawned terminal {id} on satellite {host}"),
            None => outln!("spawned terminal {id}"),
        }
        ExitCode::SUCCESS
    }
}

/// Map the typed `SpawnError` to an actionable stderr diagnostic.
pub(crate) fn report_spawn_error(err: &SpawnError) {
    match err {
        SpawnError::GroupNotFound => {
            eprintln!("phux: spawn failed: server rejected the default group");
        }
        SpawnError::SpawnFailed(reason) => eprintln!("phux: spawn failed: {reason}"),
        SpawnError::UnsupportedSatelliteRoute => {
            eprintln!(
                "phux: spawn failed: no route to that satellite \
                 (is the server running with --hub, and the name in `phux satellite list`?)"
            );
        }
        SpawnError::SatelliteUnreachable(reason) => {
            eprintln!("phux: spawn failed: satellite unreachable: {reason}");
        }
        other => eprintln!("phux: spawn failed: {other:?}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::BytesMut;
    use phux_client::layout::leaves;
    use phux_client::testkit::{ScriptSpec, ScriptedServer};
    use phux_protocol::PROTOCOL_VERSION;
    use phux_protocol::caps::ServerCapabilities;
    use phux_protocol::wire::frame::{CommandValue, ErrorCode};
    use phux_protocol::wire::info::{SessionInfo, SessionSnapshot, TerminalInfo, WindowInfo};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn state(spawned_window: Option<WindowId>) -> SessionSnapshot {
        let session_one = SessionId::new(1);
        let session_two = SessionId::new(2);
        let window_one = WindowId::new(10);
        let window_two = WindowId::new(20);
        let mut panes = vec![
            TerminalInfo::new(TerminalId::local(1), window_one, 80, 24),
            TerminalInfo::new(TerminalId::local(2), window_two, 80, 24),
        ];
        if let Some(window) = spawned_window {
            panes.push(TerminalInfo::new(TerminalId::local(3), window, 80, 24));
        }
        SessionSnapshot::new(session_two, window_two, TerminalId::local(2))
            .with_sessions(vec![
                SessionInfo::new(session_one, "one"),
                SessionInfo::new(session_two, "two"),
            ])
            .with_windows(vec![
                WindowInfo::new(window_one, session_one, "one"),
                WindowInfo::new(window_two, session_two, "two"),
            ])
            .with_panes(panes)
    }

    struct MockConnection(tokio::net::UnixStream);

    impl MockConnection {
        async fn recv(&mut self) -> FrameKind {
            let mut header = [0_u8; 4];
            self.0.read_exact(&mut header).await.expect("frame header");
            let mut body = vec![0_u8; u32::from_be_bytes(header) as usize];
            self.0.read_exact(&mut body).await.expect("frame body");
            let mut framed = Vec::with_capacity(4 + body.len());
            framed.extend_from_slice(&header);
            framed.extend_from_slice(&body);
            let (frame, tail) = FrameKind::decode(&framed).expect("decode mock frame");
            assert!(tail.is_empty());
            frame
        }

        async fn send(&mut self, frame: &FrameKind) {
            let mut bytes = BytesMut::new();
            frame.encode(&mut bytes);
            self.0.write_all(&bytes).await.expect("write mock frame");
            self.0.flush().await.expect("flush mock frame");
        }
    }

    async fn accept(listener: &tokio::net::UnixListener) -> MockConnection {
        let (stream, _) = listener.accept().await.expect("accept mock client");
        let mut conn = MockConnection(stream);
        assert!(matches!(conn.recv().await, FrameKind::Hello { .. }));
        conn.send(&FrameKind::HelloOk {
            protocol_major: PROTOCOL_VERSION.major,
            protocol_minor: PROTOCOL_VERSION.minor,
            protocol_patch: PROTOCOL_VERSION.patch,
            server_caps: ServerCapabilities::new(),
            server_id: Vec::new(),
        })
        .await;
        conn
    }

    async fn reply_state(conn: &mut MockConnection, snapshot: SessionSnapshot) {
        let FrameKind::Command {
            request_id,
            command: Command::GetState { .. },
        } = conn.recv().await
        else {
            panic!("expected GET_STATE");
        };
        conn.send(&FrameKind::CommandResult {
            request_id,
            result: CommandResult::OkWith(CommandValue::State(snapshot)),
        })
        .await;
    }

    fn spawn_mock(socket_path: &Path, wrong_owner: bool) -> std::thread::JoinHandle<()> {
        let std_listener = std::os::unix::net::UnixListener::bind(socket_path).expect("bind mock");
        std_listener.set_nonblocking(true).expect("nonblocking");
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("mock runtime");
            runtime.block_on(async move {
                let listener = tokio::net::UnixListener::from_std(std_listener).expect("listener");

                let mut pre = accept(&listener).await;
                reply_state(&mut pre, state(None)).await;
                drop(pre);

                let mut spawn = accept(&listener).await;
                let FrameKind::SpawnTerminal {
                    request_id,
                    command,
                    owner_terminal,
                    ..
                } = spawn.recv().await
                else {
                    panic!("expected SPAWN_TERMINAL");
                };
                assert_eq!(owner_terminal, Some(TerminalId::local(1)));
                assert_eq!(command, Some(vec!["agent".to_owned()]));
                spawn
                    .send(&FrameKind::TerminalSpawned {
                        request_id,
                        result: SpawnResult::Ok(TerminalId::local(3)),
                    })
                    .await;
                drop(spawn);

                let mut verify = accept(&listener).await;
                let spawned_window = if wrong_owner {
                    WindowId::new(20)
                } else {
                    WindowId::new(10)
                };
                reply_state(&mut verify, state(Some(spawned_window))).await;
                drop(verify);

                if wrong_owner {
                    let mut cleanup = accept(&listener).await;
                    let FrameKind::Command {
                        request_id,
                        command: Command::KillTerminal { terminal_id },
                    } = cleanup.recv().await
                    else {
                        panic!("expected KILL_TERMINAL rollback");
                    };
                    assert_eq!(terminal_id, TerminalId::local(3));
                    cleanup
                        .send(&FrameKind::CommandResult {
                            request_id,
                            result: CommandResult::Ok,
                        })
                        .await;
                    return;
                }

                let mut layout = accept(&listener).await;
                let FrameKind::GetMetadata { request_id, .. } = layout.recv().await else {
                    panic!("expected layout GET");
                };
                layout
                    .send(&FrameKind::MetadataValue {
                        request_id,
                        value: None,
                    })
                    .await;
                let FrameKind::SetMetadata { value, .. } = layout.recv().await else {
                    panic!("expected layout SET");
                };
                let workspace = Workspace::decode_cbor(&value).expect("placed workspace");
                assert_eq!(
                    workspace.active_window().unwrap().focus,
                    Some(TerminalId::local(1))
                );
                assert_eq!(
                    leaves(workspace.active_window().unwrap().tree.as_ref().unwrap()),
                    vec![TerminalId::local(1), TerminalId::local(3)]
                );
                let FrameKind::GetMetadata { request_id, .. } = layout.recv().await else {
                    panic!("expected confirming layout GET");
                };
                layout
                    .send(&FrameKind::MetadataValue {
                        request_id,
                        value: Some(value),
                    })
                    .await;
            });
        })
    }

    fn spawn_frame() -> FrameKind {
        FrameKind::SpawnTerminal {
            request_id: 1,
            group: GroupId::new(1),
            command: Some(vec!["agent".to_owned()]),
            cwd: None,
            env: None,
            term: None,
            satellite: None,
            owner_terminal: None,
            agent_session: None,
        }
    }

    #[test]
    fn placed_spawn_verifies_ownership_before_publishing_metadata() {
        let temp = tempfile::TempDir::new().expect("tempdir");
        let socket = temp.path().join("mock.sock");
        let mock = spawn_mock(&socket, false);
        let result = dispatch_spawn_placed(
            &socket,
            spawn_frame(),
            1,
            "spawn",
            "@1",
            SpawnSplit::Vertical,
            0.3,
            None,
        );
        assert!(matches!(result, Ok(SpawnResult::Ok(id)) if id == TerminalId::local(3)));
        mock.join().expect("mock server");
    }

    #[test]
    fn old_server_ignoring_owner_is_rolled_back_before_layout_write() {
        let temp = tempfile::TempDir::new().expect("tempdir");
        let socket = temp.path().join("mock.sock");
        let mock = spawn_mock(&socket, true);
        let result = dispatch_spawn_placed(
            &socket,
            spawn_frame(),
            1,
            "spawn",
            "@1",
            SpawnSplit::Horizontal,
            0.5,
            None,
        );
        assert!(result.is_err());
        mock.join().expect("mock server");
    }

    /// Long enough that a loaded machine cannot trip it, short enough that a
    /// genuine wedge fails this test instead of hanging the run.
    const WEDGE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

    #[tokio::test]
    async fn satellite_refusal_ends_the_spawn_instead_of_wedging_it() {
        // phux-h5hj.12. `relay.rs`'s `handle_inbound` states the shape:
        // "a satellite MAY answer a relayed spawn with a generic correlated
        // ERROR instead of TERMINAL_SPAWNED". `dispatch_spawn_async` used to
        // wait on TERMINAL_SPAWNED alone, so `phux spawn --satellite build-box`
        // against such a peer printed nothing and never exited — the user's
        // only way out was Ctrl-C, which looks identical to a hung server.
        //
        // The timeout is the assertion: on the pre-fix code this test does not
        // fail, it hangs.
        let temp = tempfile::TempDir::new().expect("tempdir");
        let socket = temp.path().join("refusing.sock");
        let listener = tokio::net::UnixListener::bind(&socket).expect("bind");
        let spec = ScriptSpec::new().refuse_spawn(
            ErrorCode::UnsupportedSatelliteRoute,
            "no satellite route to build-box",
        );
        let server = tokio::spawn(async move { ScriptedServer::accept(&listener, spec).await });

        let result = tokio::time::timeout(
            WEDGE_TIMEOUT,
            dispatch_spawn_async(&socket, &spawn_frame(), None),
        )
        .await
        .expect("a refused spawn must return; a timeout here is the wedge itself")
        .expect("transport");

        // Folded into the shape a hub already produces for this case, so the
        // CLI's existing `report_spawn_error` renders it.
        match result {
            SpawnResult::Err(SpawnError::SpawnFailed(reason)) => {
                assert!(
                    reason.contains("no satellite route to build-box"),
                    "the refusal must reach the operator, got {reason:?}"
                );
            }
            other => panic!("a correlated ERROR is this spawn's answer, got {other:?}"),
        }
        server.await.expect("scripted server");
    }
}
