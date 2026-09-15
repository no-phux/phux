use std::path::{Path, PathBuf};
use std::process::ExitCode;

use phux_client::attach::AttachError;
use phux_client::layout::SplitDir;
use phux_protocol::ids::{GroupId, ResourceId, SatelliteHost};
use phux_protocol::wire::frame::{
    Command, CommandResult, CommandValue, FrameKind, SpawnError, SpawnResult, StateScope,
};
use phux_server::runtime::default_socket_path;

use crate::commands::agent::AgentSessionRecord;
use crate::commands::{
    SpawnSplit, cli_runtime, json_err, parse_selector, request_command, resolve_targets,
};

/// `phux spawn` — create a Terminal without attaching (`SPAWN_RESOURCE`,
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
    projection: Option<&str>,
    cwd: Option<String>,
    json: bool,
    socket: Option<PathBuf>,
    command: Vec<String>,
) -> ExitCode {
    let socket_path = socket.unwrap_or_else(default_socket_path);
    let request_id = 1u32;
    let frame = FrameKind::SpawnResource {
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
        // phux-a5xj: a headless `phux spawn` has no viewport and owns no
        // layout, so it has nothing honest to name here. The pane takes the
        // server default and is sized by whichever client attaches.
        initial_size: None,
        resource: None,
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
            projection,
            None,
            json,
        ),
        None => dispatch_spawn(&socket_path, &frame, "spawn", None, json),
    };
    match result {
        Ok(SpawnResult::Ok(terminal_id)) => print_spawned(&terminal_id, json),
        Ok(SpawnResult::Err(err)) => {
            report_spawn_error(&err);
            ExitCode::FAILURE
        }
        // `SpawnResult` is `#[non_exhaustive]`: a kind with no arm here is a
        // vocabulary this client does not have, i.e. version skew.
        Ok(_) => {
            eprintln!(
                "phux: {}",
                phux_client::explain::unexpected_reply("SPAWN_RESOURCE")
            );
            ExitCode::FAILURE
        }
        Err(code) => code,
    }
}

/// Send a `SPAWN_RESOURCE` frame and return the matching `RESOURCE_SPAWNED`
/// result. Shared by `phux spawn` and `phux launch` (phux-ark7) so both
/// ride the identical wire path — the server injects `PHUX_TERMINAL_ID`
/// into the spawned pane regardless of which verb requested it. The wire
/// round trip, including the optional agent-session provenance write and its
/// `KILL_RESOURCE` rollback on failure, is
/// [`phux_client::agent_session_record::spawn_with_agent_session_on`].
///
/// On a connect/transport failure this prints the `no server` diagnostic
/// (attributed to `verb`) and returns the failure [`ExitCode`] in `Err`, so
/// callers only handle the `SpawnResult` variants.
///
/// The correlation id is not a parameter: it is read out of `frame`'s own
/// `request_id` field, so the id sent and the id waited on cannot drift.
///
/// `json` selects the failure channel per the JSON error contract
/// (phux-i0e8.8.2): under `--json` a connect failure is one JSON error line
/// on stderr rather than the prose diagnostic.
pub(crate) fn dispatch_spawn(
    socket_path: &Path,
    frame: &FrameKind,
    verb: &str,
    agent_session: Option<&AgentSessionRecord>,
    json: bool,
) -> Result<SpawnResult, ExitCode> {
    let rt = cli_runtime()?;
    rt.block_on(dispatch_spawn_async(socket_path, frame, agent_session))
        .map_err(|err| json_err::report_no_server(json, &err, socket_path, verb))
}

/// Open a connection, send `frame`, and return the correlated spawn outcome.
///
/// The wire round trip — the plain spawn, plus the optional agent-session
/// provenance write and its same-connection `KILL_RESOURCE` rollback on
/// failure — is [`phux_client::agent_session_record::spawn_with_agent_session_on`],
/// which prints the spawn's own degradation notices itself (before calling
/// `persist_record`, so the two interleaved prints land in the historical
/// encounter order) rather than returning them for this wrapper to print.
pub(crate) async fn dispatch_spawn_async(
    socket_path: &Path,
    frame: &FrameKind,
    agent_session: Option<&AgentSessionRecord>,
) -> Result<SpawnResult, AttachError> {
    phux_client::agent_session_record::spawn_with_agent_session_on(
        socket_path,
        frame,
        agent_session,
    )
    .await
}

/// Resolve an explicit local owner, spawn into its exact server window, then
/// insert the returned leaf through shared `LayoutOps`
/// ([`phux_client::spawn::verify_and_publish_placement`]). If layout
/// publication fails after spawn, kill the known new Terminal before
/// returning failure.
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
    projection: Option<&str>,
    agent_session: Option<&AgentSessionRecord>,
    json: bool,
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
            Ok(CommandResult::OkWith(CommandValue::State(s))) => s,
            Ok(other) => {
                eprintln!(
                    "phux: {}",
                    phux_client::explain::explain_unexpected("GET_STATE", &other)
                );
                return Err(ExitCode::FAILURE);
            }
            Err(err) => return Err(json_err::report_no_server(json, &err, socket_path, verb)),
        };
        let candidates = resolve_targets(socket_path, &selector, &snapshot).await;
        let Some(owner) =
            crate::selector::pick_target_pane(&candidates, &snapshot.focused_resource)
        else {
            eprintln!("phux: no such target");
            return Err(ExitCode::FAILURE);
        };
        if !matches!(owner, ResourceId::Local { .. }) {
            eprintln!("phux: explicit spawn placement is local-only");
            return Err(ExitCode::FAILURE);
        }
        let Some((owner_window, session)) =
            phux_client::spawn::ownership_for_terminal(&snapshot, &owner)
        else {
            eprintln!("phux: target has no local session ownership");
            return Err(ExitCode::FAILURE);
        };
        let FrameKind::SpawnResource { owner_terminal, .. } = &mut frame else {
            eprintln!("phux: internal spawn placement error");
            return Err(ExitCode::FAILURE);
        };
        *owner_terminal = Some(owner.clone());
        let spawned = dispatch_spawn_async(socket_path, &frame, agent_session)
            .await
            .map_err(|err| json_err::report_no_server(json, &err, socket_path, verb))?;
        let SpawnResult::Ok(new_pane) = &spawned else {
            return Ok(spawned);
        };

        let dir = match split {
            SpawnSplit::Horizontal => SplitDir::Horizontal,
            SpawnSplit::Vertical => SplitDir::Vertical,
        };
        let placement = phux_client::spawn::Placement {
            owner: owner.clone(),
            owner_window,
            owner_session: session,
            new_pane: new_pane.clone(),
        };
        let mut notices = Vec::new();
        let rollback = phux_client::spawn::verify_and_publish_placement(
            socket_path,
            &placement,
            dir,
            ratio,
            projection,
            request_id.wrapping_add(1),
            &mut notices,
        )
        .await;
        for message in &notices {
            eprintln!("phux: warning: partial results — {message}");
        }
        match rollback {
            phux_client::spawn::RollbackOutcome::Placed => Ok(spawned),
            phux_client::spawn::RollbackOutcome::RolledBack { reason } => {
                eprintln!("phux: {verb} placement failed; spawned pane was removed: {reason}");
                Err(ExitCode::FAILURE)
            }
            phux_client::spawn::RollbackOutcome::RollbackUnconfirmed {
                reason,
                cleanup_note,
            } => {
                eprintln!("phux: {verb} placement failed ({reason}); {cleanup_note}");
                Err(ExitCode::FAILURE)
            }
        }
    })
}

/// Print the freshly spawned Terminal id — human line or the stable JSON
/// document (`terminal_id` is the satellite-local id when `satellite` is
/// non-null; address it through the hub as `satellite`+`terminal_id`).
fn print_spawned(terminal_id: &ResourceId, json: bool) -> ExitCode {
    let (id, host) = match terminal_id {
        ResourceId::Local { id } => (*id, None),
        ResourceId::Satellite { host, id } => (*id, Some(host.as_str())),
    };
    if json {
        let payload = spawned_json(id, host);
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
            Some(host) => {
                outln!("Created pane {host}/@{id}. Next: `phux snapshot {host}/@{id}`.");
            }
            None => outln!("Created pane @{id}. Next: `phux snapshot @{id}`."),
        }
        ExitCode::SUCCESS
    }
}

/// The `phux spawn --json` result document. Pure, so the shape (including
/// `schema_version`) is unit-testable without a server.
fn spawned_json(id: u32, host: Option<&str>) -> serde_json::Value {
    serde_json::json!({
        "schema_version": 1,
        "terminal_id": id,
        "satellite": host,
    })
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
                 (is the server running with --hub, and the name in \
                 `phux host ls --role satellite`?)"
            );
        }
        SpawnError::SatelliteUnreachable(reason) => {
            eprintln!("phux: spawn failed: satellite unreachable: {reason}");
        }
        // `SpawnError` is `#[non_exhaustive]`: a code with no arm here is a
        // vocabulary this client does not have, i.e. version skew.
        _ => eprintln!(
            "phux: spawn failed: {}",
            phux_client::explain::unexpected_reply("SPAWN_RESOURCE")
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::BytesMut;
    use phux_client::layout::leaves;
    use phux_protocol::PROTOCOL_VERSION;
    use phux_protocol::caps::{
        BootstrapCapabilities, ServerCapabilities, select_bootstrap_profile,
    };
    use phux_protocol::ids::{SessionId, WindowId};
    use phux_protocol::wire::info::{ResourceInfo, SessionInfo, SessionSnapshot, WindowInfo};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn state(spawned_window: Option<WindowId>) -> SessionSnapshot {
        let session_one = SessionId::new(1);
        let session_two = SessionId::new(2);
        let window_one = WindowId::new(10);
        let window_two = WindowId::new(20);
        let mut panes = vec![
            ResourceInfo::new(ResourceId::local(1), window_one, 80, 24),
            ResourceInfo::new(ResourceId::local(2), window_two, 80, 24),
        ];
        if let Some(window) = spawned_window {
            panes.push(ResourceInfo::new(ResourceId::local(3), window, 80, 24));
        }
        SessionSnapshot::new(session_two, window_two, ResourceId::local(2))
            .with_sessions(vec![
                SessionInfo::new(session_one, "one"),
                SessionInfo::new(session_two, "two"),
            ])
            .with_windows(vec![
                WindowInfo::new(window_one, session_one, "one"),
                WindowInfo::new(window_two, session_two, "two"),
            ])
            .with_resources(panes)
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
        let FrameKind::Hello { client_caps, .. } = conn.recv().await else {
            panic!("expected HELLO");
        };
        let (selected_profile, bootstrap_limits) =
            select_bootstrap_profile(&client_caps, &BootstrapCapabilities::new())
                .expect("fixture profiles intersect");
        conn.send(&FrameKind::HelloOk {
            protocol_major: PROTOCOL_VERSION.major,
            protocol_minor: PROTOCOL_VERSION.minor,
            protocol_patch: PROTOCOL_VERSION.patch,
            server_caps: ServerCapabilities::new(),
            server_id: Vec::new(),
            selected_profile,
            bootstrap_limits,
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
                let FrameKind::SpawnResource {
                    request_id,
                    command,
                    owner_terminal,
                    ..
                } = spawn.recv().await
                else {
                    panic!("expected SPAWN_RESOURCE");
                };
                assert_eq!(owner_terminal, Some(ResourceId::local(1)));
                assert_eq!(command, Some(vec!["agent".to_owned()]));
                spawn
                    .send(&FrameKind::ResourceSpawned {
                        request_id,
                        result: SpawnResult::Ok(ResourceId::local(3)),
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
                        command: Command::KillResource { terminal_id },
                    } = cleanup.recv().await
                    else {
                        panic!("expected KILL_RESOURCE rollback");
                    };
                    assert_eq!(terminal_id, ResourceId::local(3));
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
                let workspace =
                    phux_client::layout::Workspace::decode_cbor(&value).expect("placed workspace");
                assert_eq!(
                    workspace.active_window().unwrap().focus,
                    Some(ResourceId::local(1))
                );
                assert_eq!(
                    leaves(workspace.active_window().unwrap().tree.as_ref().unwrap()),
                    vec![ResourceId::local(1), ResourceId::local(3)]
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

    /// `phux spawn --json` pins `schema_version` 1 plus the two documented
    /// fields (§4.11) for both a local and a satellite-routed spawn.
    #[test]
    fn spawned_json_pins_the_contract_shape() {
        let doc = spawned_json(7, None);
        assert_eq!(doc["schema_version"], 1);
        assert_eq!(doc["terminal_id"], 7);
        assert!(doc["satellite"].is_null());
        assert_eq!(doc.as_object().map(serde_json::Map::len), Some(3));

        let doc = spawned_json(3, Some("edge"));
        assert_eq!(doc["satellite"], "edge");
    }

    fn spawn_frame() -> FrameKind {
        FrameKind::SpawnResource {
            request_id: 1,
            group: GroupId::new(1),
            command: Some(vec!["agent".to_owned()]),
            cwd: None,
            env: None,
            term: None,
            satellite: None,
            owner_terminal: None,
            agent_session: None,
            initial_size: None,
            resource: None,
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
            None,
            false,
        );
        assert!(matches!(result, Ok(SpawnResult::Ok(id)) if id == ResourceId::local(3)));
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
            None,
            false,
        );
        assert!(result.is_err());
        mock.join().expect("mock server");
    }
}
