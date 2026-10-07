use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use phux_client::attach::connection::Connection;
use phux_client::session::{
    AtomicPreflightOutcome, CreateEmptyOutcome, CreateOutcome, CreateSessionError,
    CreateSessionRequest,
};
use phux_protocol::caps::ServerFeature;
use phux_protocol::ids::IdempotencyKey;
use phux_protocol::wire::frame::AttachTarget;

use crate::commands::server_target::{ServerSpec, ServerTarget};
use crate::commands::{
    attach::client_cwd, attach::configured_session_name_template,
    attach::interactive_tty_preflight, attach::render_default_session_name,
    attach::report_attach_end, attach::run_attach_once, partial, server::ensure_server,
    server::ensure_server_unseeded,
};

/// How many generated names an omitted-name `phux new` tries before falling
/// back to a numeric suffix.
const RANDOM_NAME_ATTEMPTS: usize = 8;

/// `phux new` — create a *new* session and attach to it.
///
/// The name comes from the positional `NAME` or `-s` (a conflict is an error).
/// A name that already exists is refused; an omitted one renders
/// `session-name-template`, redrawing a taken `${random-name}` and settling on a
/// numeric suffix. The create+attach rides `CreateIfMissing`. Only a local
/// server is auto-spawned. `mode.empty` (ADR-0105) creates a keep-empty session
/// with no terminal.
pub(crate) fn run_new(
    name: Option<String>,
    session: Option<String>,
    cwd: Option<PathBuf>,
    server: ServerSpec,
    mode: NewMode,
    command: Vec<String>,
    env: Vec<(String, String)>,
) -> ExitCode {
    let NewMode {
        json,
        empty,
        idempotency_key,
    } = mode;
    let requested = match requested_session_name(name, session) {
        Ok(requested) => requested,
        Err(code) => return code,
    };
    if let Some(name) = &requested
        && let Err(invalid) = phux_client::rename::check_session_name(name)
    {
        return report_invalid_session_name(json, name, &invalid);
    }

    if !json && let Err(code) = interactive_tty_preflight() {
        return code;
    }

    let (rt, target) = match prepare_target(server, json) {
        Ok(prepared) => prepared,
        Err(code) => return code,
    };

    if !json {
        if empty {
            return run_new_empty_attached(&rt, &target, requested);
        }
        return run_new_attached(&rt, &target, requested, command, cwd);
    }
    // Clap enforces `--json` => `-s NAME`; this guard only keeps a future caller
    // from panicking.
    let Some(name) = requested else {
        eprintln!("phux: `phux new --json` requires an explicit -s NAME");
        return ExitCode::from(2);
    };
    if empty {
        return run_new_empty_json(&rt, &target, &name);
    }
    run_new_json(&rt, &target, &name, cwd, command, env, idempotency_key)
}

/// How `phux new` runs: headless (`--json`), whether the session starts
/// with no terminal (`--empty`, ADR-0105), and whether the create is keyed
/// (`--idempotency-key`, ADR-0126).
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct NewMode {
    /// `--json`: create without attaching and print a document.
    pub(crate) json: bool,
    /// `--empty`: create a keep-empty session with zero windows.
    pub(crate) empty: bool,
    /// `--idempotency-key`, already parsed: the create's `request_token`.
    pub(crate) idempotency_key: Option<IdempotencyKey>,
}

/// `phux new --empty [NAME]`: create an empty session, then attach to it. The
/// TUI renders its empty state, from which a new window starts a terminal.
fn run_new_empty_attached(
    rt: &tokio::runtime::Runtime,
    target: &ServerTarget,
    requested: Option<String>,
) -> ExitCode {
    let existing = existing_session_names(rt, target);
    let name = match choose_session_name(requested, &existing) {
        Ok(name) => name,
        Err(code) => return code,
    };
    ensure_local_unseeded_server(target, false);
    if let Err(code) = rt.block_on(create_empty_session_via_metadata(target, &name, false)) {
        return code;
    }
    let dial = target.dial();
    let predict_cfg = super::attach::predictive_config_for(&dial);
    match rt.block_on(run_attach_once(
        &dial,
        AttachTarget::ByName(name.clone()),
        predict_cfg,
    )) {
        Ok(attach_end) => {
            report_attach_end(attach_end);
            ExitCode::SUCCESS
        }
        Err(err) => {
            target.report_attach_failure(&err, &name);
            ExitCode::FAILURE
        }
    }
}

/// `phux new --empty --json -s NAME`: create an empty session without
/// attaching and print [`empty_session_json`].
fn run_new_empty_json(rt: &tokio::runtime::Runtime, target: &ServerTarget, name: &str) -> ExitCode {
    ensure_local_unseeded_server(target, true);
    match rt.block_on(create_empty_session_via_metadata(target, name, true)) {
        Ok(()) => crate::output::json(&empty_session_json(name)),
        Err(code) => code,
    }
}

/// Make sure a local server is running for a create-without-attach (`phux
/// new --json`, `phux new --empty`, `phux worktree new --json`). A server
/// started here carries no seed session (ADR-0105): the requested session is
/// the only one, so a requested name of `default` does not collide with a
/// seed and no stray shell is left behind. Failure is only logged (never
/// prose under `--json`); the create reports, its JSON error carrying the
/// failure as `error.auto_start_error`.
pub(crate) fn ensure_local_unseeded_server(target: &ServerTarget, json: bool) {
    if let Some(path) = target.socket_path()
        && let Err(err) = ensure_server_unseeded(path, json)
    {
        tracing::debug!(error = %err, "auto-spawn failed on a create-without-attach path");
    }
}

/// The session name from the positional NAME or `-s`; a conflict is
/// rejected.
fn requested_session_name(
    name: Option<String>,
    session: Option<String>,
) -> Result<Option<String>, ExitCode> {
    match (name, session) {
        (Some(positional), Some(flag)) if positional != flag => {
            eprintln!(
                "phux: conflicting session names: '{positional}' (positional) vs '{flag}' (-s) — pass just one"
            );
            Err(ExitCode::FAILURE)
        }
        (Some(positional), _) => Ok(Some(positional)),
        (None, flag) => Ok(flag),
    }
}

/// Refuse a name no selector could reach again (`phux attach x:y` reads a
/// window of `x`): exit 2, before any server is dialed or spawned.
fn report_invalid_session_name(
    json: bool,
    name: &str,
    invalid: &phux_client::rename::SessionNameError,
) -> ExitCode {
    use crate::commands::json_err::{CliError, codes, emit};
    emit(
        json,
        &CliError::new(
            codes::INVALID_SESSION_NAME,
            format!("invalid session name {name:?}: {invalid}"),
            "pick a non-empty name without a leading `@`, `#`, or `%`, and without `:` or `/@`",
        ),
        2,
    )
}

/// Resolve the server `phux new` talks to; a socket path too long for
/// `sockaddr_un` fails here with the limit named.
fn prepare_target(
    server: ServerSpec,
    json: bool,
) -> Result<(tokio::runtime::Runtime, ServerTarget), ExitCode> {
    let (rt, target) = server.prepare("new", json)?;
    if let Some(path) = target.socket_path() {
        crate::commands::ensure_socket_path_fits(path)?;
    }
    Ok((rt, target))
}

/// Create a session and attach to it: the interactive `phux new`.
fn run_new_attached(
    rt: &tokio::runtime::Runtime,
    target: &ServerTarget,
    requested: Option<String>,
    command: Vec<String>,
    cwd: Option<PathBuf>,
) -> ExitCode {
    let existing = existing_session_names(rt, target);
    let name = match choose_session_name(requested, &existing) {
        Ok(name) => name,
        Err(code) => return code,
    };

    // phux-07y: `phux new` never seeds with spawn-on-attach — an
    // explicitly-created session gets a plain shell (or the `-- CMD`
    // the user gave, applied per-session via CreateIfMissing).
    if let Some(path) = target.socket_path()
        && let Err(err) = ensure_server(path, &name, None, false)
    {
        eprintln!("phux: auto-spawn skipped ({err}). Start a server manually with `phux server`.");
    }

    let session_target =
        new_session_target(name.clone(), command, seed_cwd(cwd, target.is_remote()));

    // The shared resolver picks the per-dial default: prediction off over
    // the local socket unless the config asks for it, on for a remote dial
    // that crosses a network.
    let dial = target.dial();
    let predict_cfg = super::attach::predictive_config_for(&dial);
    match rt.block_on(run_attach_once(&dial, session_target, predict_cfg)) {
        Ok(attach_end) => {
            // phux-i0e8.2.2: same one-line ending explanation as `phux
            // attach` — a last-pane death is named, a detach stays quiet.
            report_attach_end(attach_end);
            ExitCode::SUCCESS
        }
        Err(err) => {
            target.report_attach_failure(&err, &name);
            ExitCode::FAILURE
        }
    }
}

/// The session names already on the server, for duplicate rejection and
/// auto-suffixing. No server (or no answer) means none.
fn existing_session_names(rt: &tokio::runtime::Runtime, target: &ServerTarget) -> Vec<String> {
    if target.socket_path().is_some_and(|path| !path.exists()) {
        return Vec::new();
    }
    rt.block_on(target.get_state()).map_or_else(
        |_| Vec::new(),
        // Session names are hub-local, so an unreachable satellite cannot hide a
        // duplicate: warn, don't refuse.
        |view| {
            partial::warn_partial_view("new", view.degradation());
            view.snapshot()
                .sessions
                .iter()
                .map(|session| session.name.clone())
                .collect()
        },
    )
}

/// The name to create: the requested one unless it is taken, else the
/// configured template made unique.
fn choose_session_name(requested: Option<String>, existing: &[String]) -> Result<String, ExitCode> {
    let Some(requested) = requested else {
        // No name given: render the session-name-template and disambiguate.
        // `${cwd-basename}` uses the local directory even for a remote server.
        return Ok(fresh_session_name(
            existing,
            &configured_session_name_template(),
            &std::env::current_dir().unwrap_or_default(),
            &mut phux_config::NameRng::from_entropy(),
        ));
    };
    if existing.contains(&requested) {
        eprintln!(
            "phux: session '{requested}' already exists (use `phux attach {requested}` to join it)"
        );
        return Err(ExitCode::FAILURE);
    }
    Ok(requested)
}

/// `phux new --json` — create a session *without* attaching and print its
/// seed pane's id. The create is an L3 `SET_METADATA` write of
/// `phux.session.create/v1` via [`phux_client::session::create_session`]; the
/// seed-pane id is read back from the nonce-correlated result key. Requires
/// `-s NAME` (clap-enforced); a name in use is an error.
pub(crate) fn run_new_json(
    rt: &tokio::runtime::Runtime,
    target: &ServerTarget,
    name: &str,
    cwd: Option<PathBuf>,
    command: Vec<String>,
    env: Vec<(String, String)>,
    idempotency_key: Option<IdempotencyKey>,
) -> ExitCode {
    // A local server must be running to host the new session; the real
    // session is then created without attaching.
    ensure_local_unseeded_server(target, true);

    let cwd = seed_cwd(cwd, target.is_remote());
    let command = if command.is_empty() {
        None
    } else {
        Some(command)
    };
    let env = env.into_iter().collect();

    match rt.block_on(create_session_via_metadata(
        target,
        name,
        command,
        cwd,
        env,
        None,
        false,
        true,
        idempotency_key,
    )) {
        Ok(terminal_id) => crate::output::json(&new_session_json(name, terminal_id)),
        Err(code) => code,
    }
}

/// The `phux new --json` result document: the created session's name and its
/// seed pane's wire-local id. Pure, so the shape (including `schema_version`)
/// is unit-testable without a server.
fn new_session_json(session: &str, terminal_id: u64) -> serde_json::Value {
    serde_json::json!({
        "schema_version": 1,
        "session": session,
        "terminal_id": terminal_id,
    })
}

/// The `phux new --empty --json` result document (ADR-0105): the same keys
/// as [`new_session_json`], with `terminal_id` null because no terminal was
/// started, plus `empty` and `keep_empty`, both `true`.
fn empty_session_json(session: &str) -> serde_json::Value {
    serde_json::json!({
        "schema_version": 1,
        "session": session,
        "terminal_id": null,
        "empty": true,
        "keep_empty": true,
    })
}

/// Create an empty, keep-empty session (ADR-0105). Refuses before writing
/// when the server lacks `KeepEmptySessions` (an older one would seed a shell).
pub(crate) async fn create_empty_session_via_metadata(
    server: &ServerTarget,
    name: &str,
    json: bool,
) -> Result<(), ExitCode> {
    let mut conn = server
        .connect()
        .await
        .map_err(|err| server.report_unreachable(json, &err, "new"))?;
    // The capability check runs before the duplicate-name `GET_STATE`, so an
    // unsupporting server costs exactly one round trip.
    if !phux_client::session::keep_empty_supported(&conn) {
        return Err(report_empty_unsupported(json));
    }
    reject_duplicate_session_name(&mut conn, server, name, json).await?;
    let mut notices = Vec::new();
    let result = phux_client::session::create_empty_session(&mut conn, name, &mut notices).await;
    drop(conn);
    // Printed before the error is reported, in encounter order: a notice
    // collected before a later transport failure must not be dropped.
    warn_partial_results(&notices);
    let outcome = result.map_err(|err| report_create_session_error(server, json, err))?;
    match outcome {
        CreateEmptyOutcome::Created => Ok(()),
        CreateEmptyOutcome::Unsupported => Err(report_empty_unsupported(json)),
        CreateEmptyOutcome::ReadRefused(refusal) => Err(report_create_failed(
            json,
            format!("server refused the read-back: {refusal}"),
        )),
        CreateEmptyOutcome::NotRegistered => Err(report_session_not_registered(json, name)),
    }
}

/// The server cannot create a session with no terminal (ADR-0105).
fn report_empty_unsupported(json: bool) -> ExitCode {
    use crate::commands::json_err::{CliError, codes, emit};
    emit(
        json,
        &CliError::new(
            codes::UNSUPPORTED_SERVER,
            "create-session failed: the server does not support empty sessions",
            "upgrade the server (`phux upgrade`), or create the session without --empty",
        ),
        1,
    )
}

/// A create the server did not confirm, as prose or the `--json` error
/// object (`session_create_failed`, exit 1).
fn report_create_failed(json: bool, detail: impl std::fmt::Display) -> ExitCode {
    report_create_failed_with(
        json,
        detail,
        "run `phux ls` to see whether the session exists; the server log has the cause",
    )
}

/// [`report_create_failed`] with a specific remedy.
fn report_create_failed_with(json: bool, detail: impl std::fmt::Display, remedy: &str) -> ExitCode {
    use crate::commands::json_err::{CliError, codes, emit};
    emit(
        json,
        &CliError::new(
            codes::SESSION_CREATE_FAILED,
            format!("create-session failed: {detail}"),
            remedy,
        ),
        1,
    )
}

/// Ask the connected server whether it supports atomic agent-session
/// restore, via [`phux_client::session::atomic_agent_session_preflight`].
async fn require_atomic_agent_session_create(
    conn: &mut Connection,
    server: &ServerTarget,
    json: bool,
) -> Result<(), ExitCode> {
    let mut notices = Vec::new();
    // Native restore must be atomic with session creation; the preflight
    // detects servers that cannot install `agent_session` in the create.
    let result = phux_client::session::atomic_agent_session_preflight(conn, &mut notices).await;
    // Printed before the error is reported, in encounter order: a notice
    // collected before a later transport failure must not be dropped.
    warn_partial_results(&notices);
    let outcome = result.map_err(|err| server.report_unreachable(json, &err, "new"))?;
    match outcome {
        AtomicPreflightOutcome::Supported => Ok(()),
        AtomicPreflightOutcome::Unsupported => Err(report_create_failed(
            json,
            "server does not support atomic agent-session restore",
        )),
        AtomicPreflightOutcome::Refused(refusal) => Err(report_create_failed(
            json,
            format!("server refused the agent-session capability probe: {refusal}"),
        )),
    }
}

pub(crate) async fn preflight_atomic_agent_session_create(
    socket_path: &Path,
) -> Result<(), ExitCode> {
    let server = ServerTarget::local(socket_path);
    let mut conn = server
        .connect()
        .await
        .map_err(|err| server.report_unreachable(false, &err, "workspace restore"))?;
    require_atomic_agent_session_create(&mut conn, &server, false).await
}

/// Create a named session without attaching via
/// [`phux_client::session::create_session`] and map the typed outcome to this
/// verb's stderr diagnostics. `agent_session_preflighted` is true when a batch
/// caller already ran [`preflight_atomic_agent_session_create`]. Returns the
/// seed pane's id, or the (already reported) failure code.
#[allow(
    clippy::too_many_arguments,
    reason = "the shared create-without-attach path keeps the complete operation explicit"
)]
pub(crate) async fn create_session_via_metadata(
    server: &ServerTarget,
    name: &str,
    command: Option<Vec<String>>,
    cwd: Option<String>,
    env: BTreeMap<String, String>,
    agent_session: Option<Vec<u8>>,
    agent_session_preflighted: bool,
    json: bool,
    idempotency_key: Option<IdempotencyKey>,
) -> Result<u64, ExitCode> {
    let allow_legacy_result = agent_session.is_none();

    let mut conn = server
        .connect()
        .await
        .map_err(|err| server.report_unreachable(json, &err, "new"))?;

    // A keyed retry names a session the first attempt already created, so
    // the client-side duplicate check would refuse the very retry the key
    // exists for; the server answers it from its dedupe cache instead.
    match idempotency_key {
        Some(_) => refuse_unkeyed_server(&conn, json)?,
        None => reject_duplicate_session_name(&mut conn, server, name, json).await?,
    }

    let request = CreateSessionRequest {
        name,
        command: command.as_deref(),
        cwd: cwd.as_deref(),
        env: &env,
        agent_session: agent_session.as_deref(),
        idempotency_key,
    };
    let mut notices = Vec::new();
    let result = phux_client::session::create_session(
        &mut conn,
        &request,
        allow_legacy_result,
        agent_session_preflighted,
        &mut notices,
    )
    .await;
    drop(conn);
    // Printed before the error is reported, in encounter order: a notice
    // collected before a later transport failure must not be dropped.
    warn_partial_results(&notices);
    let outcome = result.map_err(|err| report_create_session_error(server, json, err))?;
    match outcome {
        CreateOutcome::Created(id) => Ok(id),
        CreateOutcome::AtomicRestoreUnsupported => Err(report_create_failed(
            json,
            "server does not support atomic agent-session restore",
        )),
        CreateOutcome::ProbeRefused(refusal) => Err(report_create_failed(
            json,
            format!("server refused the agent-session capability probe: {refusal}"),
        )),
        CreateOutcome::ReadRefused(refusal) => Err(report_create_failed(
            json,
            format!("server refused the read-back: {refusal}"),
        )),
        CreateOutcome::LegacyReadRefused(refusal) => Err(report_create_failed(
            json,
            format!("server refused legacy read-back: {refusal}"),
        )),
        CreateOutcome::NotRegistered if idempotency_key.is_some() => {
            Err(report_keyed_session_not_registered(json, name))
        }
        CreateOutcome::NotRegistered => Err(report_session_not_registered(json, name)),
    }
}

/// Map a [`CreateSessionError`] to the failure code, attributing transport
/// failures to `"new"`.
fn report_create_session_error(
    server: &ServerTarget,
    json: bool,
    err: CreateSessionError,
) -> ExitCode {
    match err {
        CreateSessionError::Attach(err) => server.report_unreachable(json, &err, "new"),
        CreateSessionError::Encode(err) => report_create_failed(
            json,
            format!("failed to serialize the create request: {err}"),
        ),
        CreateSessionError::Layout(err) => crate::commands::json_err::emit(
            json,
            &crate::commands::json_err::CliError::new(
                crate::commands::json_err::codes::LAYOUT_REJECTED,
                format!("session was created but its initial layout could not be stored: {err}"),
                "inspect `phux ls`; if the session exists, attach once or place its pane explicitly",
            ),
            1,
        ),
    }
}

/// Reject a duplicate name before writing (the server refuses silently:
/// `SET_METADATA` has no reply). The partial-fleet notice goes to stderr.
async fn reject_duplicate_session_name(
    conn: &mut Connection,
    server: &ServerTarget,
    name: &str,
    json: bool,
) -> Result<(), ExitCode> {
    let (pre, degradation) = phux_client::state::get_state_on(conn)
        .await
        .map_err(|err| server.report_unreachable(json, &err, "new"))?
        .into_parts();
    partial::warn_partial_view("new", &degradation);
    if pre.sessions.iter().any(|s| s.name == name) {
        use crate::commands::json_err::{CliError, codes, emit};
        return Err(emit(
            json,
            &CliError::new(
                codes::SESSION_EXISTS,
                format!("session '{name}' already exists"),
                format!("attach it with `phux attach {name}`, or pick another name"),
            ),
            1,
        ));
    }
    Ok(())
}

/// Refuse a keyed create on a server that would not honor the key: it would
/// read the retry as a second create of a name already in use.
fn refuse_unkeyed_server(conn: &Connection, json: bool) -> Result<(), ExitCode> {
    if phux_client::session::keyed_create_supported(conn) {
        return Ok(());
    }
    Err(crate::commands::spawn::unsupported_server(
        json,
        ServerFeature::SpawnIdempotency,
    ))
}

/// Print every degradation notice a [`phux_client::session`] wire call
/// collected, in encounter order.
fn warn_partial_results(notices: &[String]) {
    for message in notices {
        eprintln!("phux: warning: partial results — {message}");
    }
}

/// The failure reported when no create result names the requested session.
fn report_session_not_registered(json: bool, name: &str) -> ExitCode {
    report_create_failed(json, format!("server did not register session '{name}'"))
}

/// A keyed create that registered nothing: a key reused for a different
/// request (ADR-0126) and a name already in use read the same.
fn report_keyed_session_not_registered(json: bool, name: &str) -> ExitCode {
    report_create_failed_with(
        json,
        format!("server did not register session '{name}'"),
        "with --idempotency-key: the key may already belong to a different create \
         request, or the name may be in use; reuse a key only to retry the identical \
         create, and draw a fresh one for a new request",
    )
}

/// The seed pane's working directory: an explicit `--cwd`, else the client's
/// cwd for a local server (so directory-keyed tools such as `claude --resume`
/// work). A remote server gets no default.
fn seed_cwd(explicit: Option<PathBuf>, remote: bool) -> Option<String> {
    let explicit = explicit.map(|path| path.to_string_lossy().into_owned());
    if remote {
        return explicit;
    }
    explicit.or_else(client_cwd)
}

/// Build the `CreateIfMissing` target from a resolved seed cwd; the server
/// falls back to its default directory for an unusable path.
fn new_session_target(name: String, command: Vec<String>, cwd: Option<String>) -> AttachTarget {
    AttachTarget::CreateIfMissing {
        name,
        command: if command.is_empty() {
            None
        } else {
            Some(command)
        },
        cwd,
    }
}

/// A session name for an omitted-name `phux new` not in `existing`: render
/// once, redraw a taken `${random-name}` up to [`RANDOM_NAME_ATTEMPTS`] times,
/// else suffix the first render via [`unique_session_name`].
fn fresh_session_name(
    existing: &[String],
    template: &str,
    cwd: &Path,
    rng: &mut phux_config::NameRng,
) -> String {
    let first = render_default_session_name(template, cwd, rng);
    if !is_taken(existing, &first) {
        return first;
    }
    if phux_config::template_has_random_name(template)
        && let Some(retry) = retry_random_name(existing, template, cwd, rng)
    {
        return retry;
    }
    unique_session_name(existing, &first)
}

/// Re-render a `${random-name}` template until a pick is free, spending the
/// attempts left after the first render.
fn retry_random_name(
    existing: &[String],
    template: &str,
    cwd: &Path,
    rng: &mut phux_config::NameRng,
) -> Option<String> {
    (1..RANDOM_NAME_ATTEMPTS)
        .map(|_| render_default_session_name(template, cwd, rng))
        .find(|candidate| !is_taken(existing, candidate))
}

fn is_taken(existing: &[String], name: &str) -> bool {
    existing.iter().any(|e| e == name)
}

/// `base` if free, else the first free `base-2`, `base-3`, ...
pub(crate) fn unique_session_name(existing: &[String], base: &str) -> String {
    if !existing.iter().any(|e| e == base) {
        return base.to_owned();
    }
    let mut n: u32 = 2;
    loop {
        let candidate = format!("{base}-{n}");
        if !existing.iter().any(|e| e == &candidate) {
            return candidate;
        }
        n = n.saturating_add(1);
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AttachTarget, Path, PathBuf, RANDOM_NAME_ATTEMPTS, ServerTarget, choose_session_name,
        create_empty_session_via_metadata, empty_session_json, fresh_session_name,
        new_session_json, new_session_target, requested_session_name, seed_cwd,
        unique_session_name,
    };

    /// ADR-0105: `phux new --empty --json` keeps the three documented keys, with
    /// `terminal_id` null, and adds `empty` and `keep_empty`.
    #[test]
    fn empty_session_json_pins_the_contract_shape() {
        let doc = empty_session_json("parked");
        assert_eq!(doc["schema_version"], 1);
        assert_eq!(doc["session"], "parked");
        assert!(doc["terminal_id"].is_null());
        assert_eq!(doc["empty"], true);
        assert_eq!(doc["keep_empty"], true);
        assert_eq!(doc.as_object().map(serde_json::Map::len), Some(5));
    }

    /// Against a server without `KeepEmptySessions`, `phux new --empty` refuses
    /// on the capability check alone, with no `GET_STATE`.
    #[tokio::test]
    async fn create_empty_session_refuses_before_any_get_state_when_unsupported() {
        use phux_client::testkit::{ScriptSpec, ScriptedServer};
        use phux_protocol::wire::frame::FrameKind;

        let dir = tempfile::tempdir().expect("temp dir");
        let socket = dir.path().join("phux.sock");
        let listener = std::os::unix::net::UnixListener::bind(&socket).expect("bind");
        listener.set_nonblocking(true).expect("nonblocking");
        let listener = tokio::net::UnixListener::from_std(listener).expect("tokio listener");
        // A bare `ScriptSpec` advertises no `KeepEmptySessions` bit.
        let spec = ScriptSpec::new();
        let server = tokio::spawn(async move { ScriptedServer::accept(&listener, spec).await });

        let target = ServerTarget::local(&socket);
        let result = create_empty_session_via_metadata(&target, "parked", false).await;
        let seen = server.await.expect("scripted server task");

        assert_eq!(result, Err(std::process::ExitCode::FAILURE));
        assert_eq!(
            seen.len(),
            1,
            "expected only the HELLO handshake, no GET_STATE; sent {seen:?}"
        );
        assert!(matches!(seen[0], FrameKind::Hello { .. }));
    }

    use phux_config::{NameRng, random_name};

    const CWD: &str = "/home/me/proj";

    /// The first `count` names a generator seeded with `seed` will produce.
    fn picks(seed: u64, count: usize) -> Vec<String> {
        let mut rng = NameRng::seeded(seed);
        (0..count).map(|_| random_name(&mut rng)).collect()
    }

    /// A free generated name is taken as-is on the first draw.
    #[test]
    fn fresh_session_name_takes_a_free_random_pick() {
        let expected = picks(4, 1).remove(0);
        let name = fresh_session_name(
            &["other".to_owned()],
            "${random-name}",
            Path::new(CWD),
            &mut NameRng::seeded(4),
        );
        assert_eq!(name, expected);
    }

    /// A taken pick is retried with the next draw rather than suffixed.
    #[test]
    fn fresh_session_name_retries_a_taken_random_pick() {
        let sequence = picks(4, 2);
        assert_ne!(sequence[0], sequence[1], "seed must yield distinct picks");
        let name = fresh_session_name(
            &[sequence[0].clone()],
            "${random-name}",
            Path::new(CWD),
            &mut NameRng::seeded(4),
        );
        assert_eq!(name, sequence[1]);
    }

    /// Every attempt taken ⇒ the first pick gets the numeric suffix, so
    /// uniqueness holds however crowded the name space is.
    #[test]
    fn fresh_session_name_falls_back_to_a_numeric_suffix() {
        let existing = picks(4, RANDOM_NAME_ATTEMPTS);
        let name = fresh_session_name(
            &existing,
            "${random-name}",
            Path::new(CWD),
            &mut NameRng::seeded(4),
        );
        assert_eq!(name, format!("{}-2", existing[0]));
        assert!(!existing.contains(&name));
    }

    /// A generated name obeys the rule an explicit one does: `phux new` run
    /// in a directory named `@proj` made a session `@proj`, which every
    /// selector reads as a pane id.
    #[test]
    fn fresh_session_name_is_addressable_whatever_the_directory() {
        let mut rng = NameRng::seeded(4);
        for (dir, template, expected) in [
            ("/work/@proj", "${cwd-basename}", "_proj"),
            ("/work/#notes", "${cwd-basename}", "_notes"),
            ("/work/a:b", "${cwd-basename}", "a_b"),
            ("/work/proj", "main:${cwd-basename}", "main_proj"),
        ] {
            let name = fresh_session_name(&[], template, Path::new(dir), &mut rng);
            assert_eq!(name, expected, "{dir} {template}");
            assert_eq!(phux_client::rename::check_session_name(&name), Ok(()));
        }
    }

    /// A deterministic template keeps the historical `base`, `base-2` shape.
    #[test]
    fn fresh_session_name_suffixes_a_deterministic_template() {
        let mut rng = NameRng::seeded(4);
        assert_eq!(
            fresh_session_name(&[], "default", Path::new(CWD), &mut rng),
            "default"
        );
        assert_eq!(
            fresh_session_name(
                &["proj".to_owned()],
                "${cwd-basename}",
                Path::new(CWD),
                &mut rng
            ),
            "proj-2"
        );
    }

    /// `phux new --json` pins `schema_version` 1 plus the two documented
    /// fields — the shape `docs/consumers/agents.md` §4.4 promises.
    #[test]
    fn new_session_json_pins_the_contract_shape() {
        let doc = new_session_json("work", 2);
        assert_eq!(doc["schema_version"], 1);
        assert_eq!(doc["session"], "work");
        assert_eq!(doc["terminal_id"], 2);
        assert_eq!(doc.as_object().map(serde_json::Map::len), Some(3));
    }

    /// phux-0db: `phux new` without `--cwd` seeds a local session in the
    /// *client's* cwd, not `None` (= the daemon's CWD).
    #[test]
    fn seed_cwd_defaults_a_local_session_to_the_client_cwd() {
        let expected = std::env::current_dir()
            .expect("test cwd")
            .to_string_lossy()
            .into_owned();
        assert_eq!(seed_cwd(None, false), Some(expected));
    }

    /// A remote server gets no client-cwd default (a local path names
    /// nothing there), but an explicit `--cwd` is still sent verbatim.
    #[test]
    fn seed_cwd_sends_only_an_explicit_cwd_to_a_remote() {
        assert_eq!(seed_cwd(None, true), None);
        assert_eq!(
            seed_cwd(Some(PathBuf::from("/srv/work")), true),
            Some("/srv/work".to_owned())
        );
    }

    /// An explicit `--cwd` wins over the client-cwd default, and a
    /// non-empty command rides along unchanged.
    #[test]
    fn new_session_target_honors_explicit_cwd_and_command() {
        assert_eq!(
            new_session_target(
                "proj".to_owned(),
                vec!["vim".to_owned(), "notes.txt".to_owned()],
                seed_cwd(Some(PathBuf::from("/somewhere/else")), false),
            ),
            AttachTarget::CreateIfMissing {
                name: "proj".to_owned(),
                command: Some(vec!["vim".to_owned(), "notes.txt".to_owned()]),
                cwd: Some("/somewhere/else".to_owned()),
            }
        );
    }

    /// The positional and `-s` spellings agree or conflict; they never
    /// silently pick one.
    #[test]
    fn requested_session_name_merges_the_two_spellings() {
        assert_eq!(
            requested_session_name(Some("a".to_owned()), None).ok(),
            Some(Some("a".to_owned()))
        );
        assert_eq!(
            requested_session_name(None, Some("b".to_owned())).ok(),
            Some(Some("b".to_owned()))
        );
        assert_eq!(
            requested_session_name(Some("a".to_owned()), Some("a".to_owned())).ok(),
            Some(Some("a".to_owned()))
        );
        assert!(requested_session_name(Some("a".to_owned()), Some("b".to_owned())).is_err());
        assert_eq!(requested_session_name(None, None).ok(), Some(None));
    }

    /// A taken name is refused; a free one is kept.
    #[test]
    fn choose_session_name_refuses_a_duplicate() {
        let existing = vec!["work".to_owned()];
        assert!(choose_session_name(Some("work".to_owned()), &existing).is_err());
        assert_eq!(
            choose_session_name(Some("play".to_owned()), &existing).ok(),
            Some("play".to_owned())
        );
    }

    #[test]
    fn unique_session_name_uses_the_base_then_numeric_suffixes() {
        // Free base ⇒ the base verbatim (no "-2" churn, no bare "0").
        assert_eq!(unique_session_name(&[], "default"), "default");
        assert_eq!(
            unique_session_name(&["other".to_owned()], "default"),
            "default",
        );
        // Base taken ⇒ first free `base-N`, starting at 2.
        assert_eq!(
            unique_session_name(&["default".to_owned()], "default"),
            "default-2",
        );
        assert_eq!(
            unique_session_name(&["default".to_owned(), "default-2".to_owned()], "default",),
            "default-3",
        );
        // A non-default template base works the same way.
        assert_eq!(unique_session_name(&["phux".to_owned()], "phux"), "phux-2");
    }
}
