use std::path::{Path, PathBuf};
use std::process::ExitCode;

use phux_client::attach::AttachError;
use phux_client::layout::SplitDir;
use phux_protocol::caps::ServerFeature;
use phux_protocol::ids::{GroupId, IdempotencyKey, ResourceId, SatelliteHost};
use phux_protocol::wire::frame::{
    Command, CommandResult, CommandValue, FrameKind, SpawnError, SpawnResource, SpawnResult,
    StateScope,
};
use phux_server::runtime::default_socket_path;

use crate::commands::agent::AgentSessionRecord;
use crate::commands::json_err::{CliError, codes};
use crate::commands::{
    SpawnSplit, cli_runtime, json_err, parse_selector, request_command, resolve_targets,
};
use crate::exit_codes::EXIT_USAGE;

/// What `phux spawn` asks the server to keep for it: the pane after its
/// process exits (`--retain`, ADR-0124), and the answer to a retry
/// (`--idempotency-key`, ADR-0126).
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct SpawnDurability {
    /// `--retain[=SECS]`; `Some(0)` asks for the server's default.
    pub(crate) retain_secs: Option<u32>,
    /// `--idempotency-key`, already parsed.
    pub(crate) idempotency_key: Option<IdempotencyKey>,
}

impl SpawnDurability {
    /// Neither flag was given: the spawn is the plain one every server
    /// understands.
    const fn is_plain(self) -> bool {
        self.retain_secs.is_none() && self.idempotency_key.is_none()
    }

    /// The spawn fields that carry it, or `None` for a plain spawn, which
    /// keeps the bytes a server without either feature always received.
    fn resource(self) -> Option<Box<SpawnResource>> {
        (self.retain_secs.is_some() || self.idempotency_key.is_some()).then(|| {
            Box::new(
                SpawnResource::default()
                    .with_retain_secs(self.retain_secs)
                    .with_idempotency_key(self.idempotency_key),
            )
        })
    }
}

/// Parse an `--idempotency-key` argument, reporting a malformed one as a
/// usage error before any connection.
pub(crate) fn parse_key_arg(
    raw: Option<&str>,
    json: bool,
) -> Result<Option<IdempotencyKey>, ExitCode> {
    raw.map(phux_client::spawn::parse_idempotency_key)
        .transpose()
        .map_err(|err| {
            json_err::emit(
                json,
                &CliError::new(
                    codes::INVALID_IDEMPOTENCY_KEY,
                    err.to_string(),
                    "generate one with `openssl rand -hex 16` and reuse it on every retry \
                     of the same request",
                ),
                EXIT_USAGE,
            )
        })
}

/// Refuse a request whose durability flag the server would silently ignore:
/// it does not advertise `missing`. Exit 2 with `unsupported_server`.
pub(crate) fn unsupported_server(json: bool, missing: ServerFeature) -> ExitCode {
    let flag = match missing {
        ServerFeature::RetainOnExit => "--retain",
        _ => "--idempotency-key",
    };
    let name = crate::feature_names::feature_name(missing).unwrap_or("the feature");
    json_err::emit(
        json,
        &CliError::new(
            codes::UNSUPPORTED_SERVER,
            format!("this server does not support {flag}: it does not advertise {name}"),
            "upgrade the server (`phux upgrade` after installing a newer phux), or drop the flag",
        ),
        EXIT_USAGE,
    )
}

/// The refusal for a spawn whose durability fields the server would skip,
/// or `None` when it can be sent. A plain spawn costs no extra connection.
fn refuse_unsupported(
    socket_path: &Path,
    durability: SpawnDurability,
    frame: &FrameKind,
    json: bool,
) -> Option<ExitCode> {
    if durability.is_plain() {
        return None;
    }
    let rt = match cli_runtime() {
        Ok(rt) => rt,
        Err(code) => return Some(code),
    };
    let features = match rt.block_on(phux_client::spawn::server_features(socket_path)) {
        Ok(features) => features,
        Err(err) => return Some(json_err::report_no_server(json, &err, socket_path, "spawn")),
    };
    let missing = phux_client::spawn::missing_spawn_feature(frame, features)?;
    Some(unsupported_server(json, missing))
}

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
    durability: SpawnDurability,
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
        resource: durability.resource(),
    };
    if let Some(code) = refuse_unsupported(&socket_path, durability, &frame, json) {
        return code;
    }
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
        Ok(SpawnResult::Ok(terminal_id)) => print_spawned(&terminal_id, false, json),
        // A keyed retry inside the server's horizon: the first spawn's pane,
        // already placed by that spawn, so nothing is placed again.
        Ok(SpawnResult::Replayed { id, .. }) => print_spawned(&id, true, json),
        Ok(SpawnResult::Err(SpawnError::IdempotencyConflict)) => json_err::emit(
            json,
            &CliError::new(
                codes::IDEMPOTENCY_CONFLICT,
                "the idempotency key was already used for a different spawn; nothing was spawned",
                "reuse a key only to retry the identical spawn; draw a fresh key for a new one",
            ),
            EXIT_USAGE,
        ),
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
/// document ([`phux_client::spawn::spawned_document`], the builder the MCP
/// `phux_spawn` tool also returns).
fn print_spawned(terminal_id: &ResourceId, replayed: bool, json: bool) -> ExitCode {
    if json {
        let payload = phux_client::spawn::spawned_document(terminal_id, replayed);
        return match serde_json::to_string_pretty(&payload) {
            Ok(s) => {
                outln!("{s}");
                ExitCode::SUCCESS
            }
            Err(err) => {
                eprintln!("phux: failed to serialize spawn result as JSON: {err}");
                ExitCode::FAILURE
            }
        };
    }
    let selector = crate::selector::format_terminal_id(terminal_id);
    let verb = if replayed {
        "Found pane (an earlier spawn with this key)"
    } else {
        "Created pane"
    };
    outln!("{verb} {selector}. Next: `phux snapshot {selector}`.");
    ExitCode::SUCCESS
}

/// Map the typed `SpawnError` to an actionable stderr diagnostic
/// ([`phux_client::spawn::spawn_error_message`]).
pub(crate) fn report_spawn_error(err: &SpawnError) {
    eprintln!("phux: {}", phux_client::spawn::spawn_error_message(err));
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
                        command: Command::KillResource { terminal_id, .. },
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
    fn spawned_document_pins_the_contract_shape() {
        let doc = phux_client::spawn::spawned_document(&ResourceId::local(7), false);
        assert_eq!(doc["schema_version"], 1);
        assert_eq!(doc["terminal_id"], 7);
        assert!(doc["satellite"].is_null());
        assert_eq!(doc["replayed"], false);
        assert_eq!(doc.as_object().map(serde_json::Map::len), Some(4));

        let doc = phux_client::spawn::spawned_document(&ResourceId::satellite("edge", 3), true);
        assert_eq!(doc["terminal_id"], 3);
        assert_eq!(doc["satellite"], "edge");
        assert_eq!(doc["replayed"], true);
    }

    #[test]
    fn durability_rides_the_spawn_only_when_asked() {
        assert!(SpawnDurability::default().resource().is_none());
        let key = phux_client::spawn::parse_idempotency_key("0123456789abcdef0123456789abcdef")
            .expect("key");
        let resource = SpawnDurability {
            retain_secs: Some(0),
            idempotency_key: Some(key),
        }
        .resource()
        .expect("fields");
        assert_eq!(resource.retain_secs, Some(0));
        assert_eq!(resource.idempotency_key, Some(key));
    }

    #[test]
    fn a_malformed_key_is_a_usage_error() {
        for bad in [
            "",
            "abc",
            "00000000000000000000000000000000",
            "zz23456789abcdef0123456789abcdef",
        ] {
            assert_eq!(
                parse_key_arg(Some(bad), true).expect_err(bad),
                ExitCode::from(EXIT_USAGE)
            );
        }
        assert_eq!(parse_key_arg(None, true), Ok(None));
    }

    #[test]
    fn retain_and_key_flags_parse_before_the_command() {
        let cli = crate::parse_cli([
            "phux",
            "spawn",
            "--retain=30",
            "--idempotency-key",
            "0123456789abcdef0123456789abcdef",
            "--",
            "true",
        ])
        .expect("parses");
        let Some(crate::commands::Command::Spawn {
            retain,
            idempotency_key,
            command,
            ..
        }) = cli.command
        else {
            panic!("expected Spawn");
        };
        assert_eq!(retain, Some(30));
        assert_eq!(
            idempotency_key.as_deref(),
            Some("0123456789abcdef0123456789abcdef")
        );
        assert_eq!(command, vec!["true".to_owned()]);

        let bare = crate::parse_cli(["phux", "spawn", "--retain", "--", "true"]).expect("parses");
        let Some(crate::commands::Command::Spawn { retain, .. }) = bare.command else {
            panic!("expected Spawn");
        };
        assert_eq!(retain, Some(0), "bare --retain asks for the server default");
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
