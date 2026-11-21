use std::path::{Path, PathBuf};
use std::process::ExitCode;

use phux_client::layout::SplitDir;
use phux_config::socket::default_socket_path;
use phux_protocol::ids::{GroupId, IdempotencyKey, ResourceId, SatelliteHost};
use phux_protocol::wire::frame::{FrameKind, SpawnError, SpawnResource, SpawnResult};

use crate::commands::json_err::{CliError, codes};
use crate::commands::{SpawnSplit, cli_runtime, json_err, parse_target};
use crate::exit_codes::EXIT_USAGE;
use phux_client::agent_session_record::{AgentSessionRecord, spawn_with_agent_session_on};
use phux_client::spawn::{PlacedSpawn, PlacedSpawnError};

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

/// What the new pane runs: `argv` (the default shell when empty), and
/// whether it is an application helper (`--app`), whose documents the
/// spawn waits for and reports.
#[derive(Debug, Default)]
pub(crate) struct SpawnCommand {
    pub(crate) app: bool,
    pub(crate) argv: Vec<String>,
}

/// How long `phux spawn --app` waits for the app's documents.
const APP_DOCUMENTS_WAIT: std::time::Duration = std::time::Duration::from_secs(20);

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

/// `phux spawn` creates a Terminal without attaching or auto-starting a
/// server. Use `RESOURCE_CREATE` with placement or `SPAWN_RESOURCE` without it.
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
    command: SpawnCommand,
    durability: SpawnDurability,
) -> ExitCode {
    let SpawnCommand { app, argv: command } = command;
    let socket_path = socket.unwrap_or_else(default_socket_path);
    let request_id = 1u32;
    let frame = FrameKind::SpawnResource {
        request_id,
        // v0.1 servers expose the single default group.
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
        Ok(SpawnResult::Ok(terminal_id)) if app => print_app(&socket_path, &terminal_id, json),
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

/// Resolve an explicit anchor and atomically create and place a Terminal.
#[allow(
    clippy::too_many_arguments,
    reason = "shared spawn placement keeps the complete CLI operation explicit"
)]
pub(crate) fn dispatch_spawn_placed(
    socket_path: &Path,
    frame: FrameKind,
    request_id: u32,
    verb: &str,
    target_text: &str,
    split: SpawnSplit,
    ratio: f32,
    projection: Option<&str>,
    agent_session: Option<&AgentSessionRecord>,
    json: bool,
) -> Result<SpawnResult, ExitCode> {
    let report = |err: PlacedSpawnError| {
        if let PlacedSpawnError::Attach(err) = &err {
            json_err::report_no_server(json, err, socket_path, verb)
        } else {
            let (refusal, exit) = err.refusal(target_text);
            json_err::emit(json, &refusal, exit)
        }
    };
    let dir = match split {
        SpawnSplit::Horizontal => SplitDir::Horizontal,
        SpawnSplit::Vertical => SplitDir::Vertical,
    };
    let spawn = PlacedSpawn::from_spawn_frame(frame, dir, ratio, projection, agent_session)
        .map_err(report)?;
    let selector = parse_target(Some(target_text)).map_err(|refusal| refusal.emit(json))?;
    let (result, degradation) = cli_runtime()?
        .block_on(spawn.run(socket_path, &selector, request_id))
        .map_err(report)?;
    if !json {
        for notice in degradation.notices() {
            warn!("{}", phux_client::state::partial_view_warning(verb, notice));
        }
    }
    Ok(result)
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

/// Report an app pane with the documents its helper opened, waiting up to
/// [`APP_DOCUMENTS_WAIT`] for them; a pane that exits first failed to start.
fn print_app(socket_path: &Path, pane: &ResourceId, json: bool) -> ExitCode {
    let rt = match cli_runtime() {
        Ok(rt) => rt,
        Err(code) => return code,
    };
    let found = rt.block_on(phux_client::app::documents_under(
        socket_path,
        pane,
        phux_client::app::APP_DOCUMENTS,
        APP_DOCUMENTS_WAIT,
    ));
    let documents = match found {
        Ok(Some(documents)) => documents,
        Ok(None) => {
            return json_err::emit(
                json,
                &CliError::new(
                    codes::SPAWN_FAILED,
                    "the app pane exited before it opened any document",
                    "run `phux agent app run NAME` inside a pane to see why",
                ),
                1,
            );
        }
        Err(err) => return json_err::report_no_server(json, &err, socket_path, "spawn"),
    };
    if json {
        let mut payload = phux_client::spawn::spawned_document(pane, false);
        payload["documents"] = serde_json::Value::Array(documents);
        return crate::output::json(&payload);
    }
    let selector = phux_client::selector::format_terminal_id(pane);
    let ids: Vec<&str> = documents
        .iter()
        .filter_map(|document| document["id"].as_str())
        .collect();
    outln!(
        "Created app pane {selector}; documents: {}. Next: `phux agent tree snapshot ID`.",
        if ids.is_empty() {
            "none yet".to_owned()
        } else {
            ids.join(" ")
        }
    );
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

/// Run `phux spawn` as parsed: refuse an `--app` the pane cannot host,
/// resolve `--idempotency-key`, then spawn.
pub(crate) fn run(args: crate::commands::SpawnArgs, socket: Option<PathBuf>) -> ExitCode {
    let crate::commands::SpawnArgs {
        satellite,
        target,
        split,
        ratio,
        projection,
        cwd,
        retain,
        idempotency_key,
        app,
        browser,
        headless,
        json,
        command,
    } = args;
    let spec = app.as_deref().map(|name| phux_client::app::AppSpec {
        name,
        browser: browser.as_deref(),
        headless,
    });
    if let Some(refusal) = spec.as_ref().and_then(phux_client::app::pane_refusal) {
        return json_err::emit(
            json.json,
            &json_err::CliError::new(
                json_err::codes::USAGE_ERROR,
                refusal,
                "pass `--browser` for a browser, which a pane can mirror",
            ),
            crate::exit_codes::EXIT_USAGE,
        );
    }
    let app = spec.map(|spec| {
        phux_client::app::helper_argv(
            &std::env::current_exe()
                .map_or_else(|_| "phux".to_owned(), |exe| exe.display().to_string()),
            spec,
        )
    });
    if app.is_some() && !command.is_empty() {
        return json_err::emit(
            json.json,
            &json_err::CliError::new(
                json_err::codes::USAGE_ERROR,
                "`--app` runs its own command; drop the command after `--`",
                "pass app arguments with `phux spawn -- phux agent app run NAME -- ARGS`",
            ),
            crate::exit_codes::EXIT_USAGE,
        );
    }
    match parse_key_arg(idempotency_key.as_deref(), json.json) {
        Ok(idempotency_key) => run_spawn(
            satellite,
            target,
            split,
            ratio,
            projection.as_deref(),
            cwd,
            json.json,
            socket,
            SpawnCommand {
                app: app.is_some(),
                argv: app.unwrap_or(command),
            },
            SpawnDurability {
                retain_secs: retain,
                idempotency_key,
            },
        ),
        Err(code) => code,
    }
}

#[cfg(test)]
mod tests;
