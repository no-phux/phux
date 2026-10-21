use std::path::{Path, PathBuf};
use std::process::ExitCode;

use phux_client::layout::SplitDir;
use phux_protocol::caps::ServerFeature;
use phux_protocol::ids::{GroupId, IdempotencyKey, ResourceId, SatelliteHost};
use phux_protocol::wire::frame::{
    Command, CommandResult, CommandValue, FrameKind, SpawnError, SpawnResource, SpawnResult,
    StateScope,
};
use phux_server::runtime::default_socket_path;

use crate::commands::json_err::{CliError, codes};
use crate::commands::{
    SpawnSplit, cli_runtime, json_err, parse_selector, request_command, resolve_targets,
};
use crate::exit_codes::EXIT_USAGE;
use phux_client::agent_session_record::{AgentSessionRecord, spawn_with_agent_session_on};

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
    let name = missing.snake_name();
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

/// `phux spawn` — create a Terminal without attaching (`SPAWN_RESOURCE`, L1
/// §3.1); never auto-starts a server. With placement the new leaf goes beside
/// the target Terminal; without, it joins the most recently active session.
/// `--satellite NAME` routes the spawn through a hub (refused on a non-hub or
/// for an unknown name). `--json` stdout is only
/// `{"terminal_id": N, "satellite": "NAME" | null}`.
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
        Ok(SpawnResult::Err(err)) => report_spawn_error(json, &err),
        // `SpawnResult` is `#[non_exhaustive]`: a kind with no arm here is a
        // vocabulary this client does not have, i.e. version skew.
        Ok(_) => {
            error!(
                "{}",
                phux_client::explain::unexpected_reply("SPAWN_RESOURCE")
            );
            ExitCode::FAILURE
        }
        Err(code) => code,
    }
}

/// Send a `SPAWN_RESOURCE` and return its `RESOURCE_SPAWNED`, shared by
/// `phux spawn` and `phux launch`. The round trip (with the optional
/// agent-session write and its rollback) is
/// [`phux_client::agent_session_record::spawn_with_agent_session_on`]; the
/// correlation id is read from `frame`. A connect failure is reported (as JSON
/// under `json`) and returned in `Err`.
pub(crate) fn dispatch_spawn(
    socket_path: &Path,
    frame: &FrameKind,
    verb: &str,
    agent_session: Option<&AgentSessionRecord>,
    json: bool,
) -> Result<SpawnResult, ExitCode> {
    let rt = cli_runtime()?;
    rt.block_on(spawn_with_agent_session_on(
        socket_path,
        frame,
        agent_session,
    ))
    .map_err(|err| json_err::report_no_server(json, &err, socket_path, verb))
}

/// Resolve an explicit local owner, spawn into its session, then insert the
/// leaf via [`phux_client::spawn::verify_and_publish_placement`]. If layout
/// publication fails, kill the new Terminal before returning failure.
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
                return Err(json_err::emit(
                    json,
                    &CliError::new(
                        codes::TRANSPORT,
                        phux_client::explain::explain_unexpected("GET_STATE", &other),
                        "run `phux doctor` for a health check",
                    ),
                    1,
                ));
            }
            Err(err) => return Err(json_err::report_no_server(json, &err, socket_path, verb)),
        };
        let candidates = resolve_targets(socket_path, &selector, &snapshot)
            .await
            .map_err(|err| crate::commands::report_agent_resolve_error(json, &err, false))?;
        let (owner, session) = placement_owner(json, target_text, &snapshot, &candidates)?;
        let FrameKind::SpawnResource { owner_terminal, .. } = &mut frame else {
            return Err(json_err::emit(
                json,
                &CliError::new(
                    codes::INTERNAL_ERROR,
                    "internal spawn placement error",
                    "report this with `phux report`",
                ),
                1,
            ));
        };
        *owner_terminal = Some(owner.clone());
        let spawned = spawn_with_agent_session_on(socket_path, &frame, agent_session)
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
        // Warnings are prose; under --json stderr carries only the one
        // error document a failure emits.
        if !json {
            for message in &notices {
                warn!("partial results — {message}");
            }
        }
        match rollback {
            phux_client::spawn::RollbackOutcome::Placed => Ok(spawned),
            phux_client::spawn::RollbackOutcome::RolledBack { reason } => Err(placement_failed(
                json,
                format!("{verb} placement failed; spawned pane was removed: {reason}"),
            )),
            phux_client::spawn::RollbackOutcome::RollbackUnconfirmed {
                reason,
                cleanup_note,
            } => Err(placement_failed(
                json,
                format!("{verb} placement failed ({reason}); {cleanup_note}"),
            )),
        }
    })
}

/// The local pane a placed spawn lands beside, with its session;
/// every refusal is reported (as JSON under `json`) and returned in `Err`.
fn placement_owner(
    json: bool,
    target_text: &str,
    snapshot: &phux_protocol::wire::info::SessionSnapshot,
    candidates: &[ResourceId],
) -> Result<(ResourceId, phux_protocol::ids::SessionId), ExitCode> {
    let Some(owner) =
        phux_client::selector::pick_target_pane(candidates, &snapshot.focused_resource)
    else {
        return Err(json_err::emit(
            json,
            &CliError::new(
                codes::NO_SUCH_TARGET,
                format!("no such target: {target_text}"),
                "run `phux ls` to see live sessions and panes",
            ),
            1,
        ));
    };
    if !matches!(owner, ResourceId::Local { .. }) {
        return Err(json_err::emit(
            json,
            &CliError::new(
                codes::SATELLITE_TARGET,
                "explicit spawn placement is local-only",
                "target a pane on this server, or run the verb on the satellite's host",
            ),
            1,
        ));
    }
    let Some(session) = phux_client::spawn::ownership_for_terminal(snapshot, &owner) else {
        return Err(placement_failed(
            json,
            "target has no local session ownership".to_owned(),
        ));
    };
    Ok((owner, session))
}

/// Print the freshly spawned Terminal id — human line or the stable JSON
/// document ([`phux_client::spawn::spawned_document`], the builder the MCP
/// `phux_spawn` tool also returns).
fn print_spawned(terminal_id: &ResourceId, replayed: bool, json: bool) -> ExitCode {
    if json {
        let payload = phux_client::spawn::spawned_document(terminal_id, replayed);
        return crate::output::json(&payload);
    }
    let selector = phux_client::selector::format_terminal_id(terminal_id);
    let verb = if replayed {
        "Found pane (an earlier spawn with this key)"
    } else {
        "Created pane"
    };
    outln!("{verb} {selector}. Next: `phux snapshot {selector}`.");
    ExitCode::SUCCESS
}

/// Map the typed `SpawnError` to an actionable diagnostic
/// ([`phux_client::spawn::spawn_error_message`]), as the JSON error document
/// under `json`. Exits 1.
pub(crate) fn report_spawn_error(json: bool, err: &SpawnError) -> ExitCode {
    if phux_client::spawn::spawn_outcome_unknown(err) {
        return json_err::emit(
            json,
            &CliError::new(
                codes::SPAWN_OUTCOME_UNKNOWN,
                phux_client::spawn::spawn_error_message(err),
                "run `phux ls` to see whether the pane exists before retrying",
            ),
            1,
        );
    }
    json_err::emit(
        json,
        &CliError::new(
            codes::SPAWN_FAILED,
            phux_client::spawn::spawn_error_message(err),
            "check the command and working directory; `phux logs` has the server's view",
        ),
        1,
    )
}

/// A placed spawn that could not land where it was asked: exit 1,
/// `spawn_failed` under `json`.
fn placement_failed(json: bool, message: String) -> ExitCode {
    json_err::emit(
        json,
        &CliError::new(
            codes::SPAWN_FAILED,
            message,
            "run `phux ls` to check the target and the pane the spawn left behind",
        ),
        1,
    )
}

#[cfg(test)]
mod tests;
