use std::path::{Path, PathBuf};
use std::process::ExitCode;

use phux_client::attach::connection::Connection;
use phux_protocol::wire::frame::{
    AttachTarget, FrameKind, SESSION_CREATE_KEY, SESSION_CREATE_RESULT_KEY,
    SESSION_CREATE_RESULT_KEY_PREFIX, Scope,
};

use crate::commands::server_target::{ServerSpec, ServerTarget};
use crate::commands::{
    DEFAULT_SESSION_NAME, attach::client_cwd, attach::configured_session_name_template,
    attach::interactive_tty_preflight, attach::render_default_session_name,
    attach::report_attach_end, attach::run_attach_once, partial, server::ensure_server,
};

/// How many generated names an omitted-name `phux new` tries against the
/// live session list before falling back to a numeric suffix. With ~8000
/// adjective-noun pairs a miss this many times in a row means the space is
/// crowded, and the suffix still guarantees a distinct name.
const RANDOM_NAME_ATTEMPTS: usize = 8;

/// `phux new` — create a *new* session and attach to it.
///
/// The name comes from the positional `NAME` or the `-s` flag (the same
/// field, two spellings; a genuine conflict is an error). "New" is enforced
/// client-side against a `GET_STATE` snapshot: a name that already exists is
/// an error (like tmux's duplicate-session refusal), and an omitted name
/// renders the configured `session-name-template` (`${cwd-basename}` by
/// default): a taken `${random-name}` pick is redrawn a few times, and a
/// numeric suffix settles anything still taken. The create+attach itself
/// rides `CreateIfMissing` (ADR-0021 defers a dedicated create-session
/// command).
///
/// `server` is the local socket or a `--remote` host (see `server_target`).
/// Only the local server is auto-spawned: a remote one is the far host's to
/// run.
pub(crate) fn run_new(
    name: Option<String>,
    session: Option<String>,
    cwd: Option<PathBuf>,
    server: ServerSpec,
    json: bool,
    command: Vec<String>,
    env: Vec<(String, String)>,
) -> ExitCode {
    let requested = match requested_session_name(name, session) {
        Ok(requested) => requested,
        Err(code) => return code,
    };

    if !json && let Err(code) = interactive_tty_preflight() {
        return code;
    }

    let (rt, target) = match prepare_target(server, json) {
        Ok(prepared) => prepared,
        Err(code) => return code,
    };

    if !json {
        return run_new_attached(&rt, &target, requested, command, cwd);
    }
    // The `--json` ⇒ `-s NAME` rule is clap-enforced on the `new` verb
    // (phux-i0e8.8.4), so the CLI cannot reach this guard; it exists
    // only so a future non-clap caller fails with the same exit code
    // clap's usage error carries, never a panic.
    let Some(name) = requested else {
        eprintln!("phux: `phux new --json` requires an explicit -s NAME");
        return ExitCode::from(2);
    };
    run_new_json(&rt, &target, &name, cwd, command, env)
}

/// The session name from the positional NAME or the `-s` flag.
///
/// They are the same field with two spellings. A genuine conflict is
/// rejected rather than silently resolved in favor of one.
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

/// Resolve the server `phux new` talks to.
///
/// phux-iwuc: a local socket path too long for `sockaddr_un` fails here,
/// with the limit named, instead of after the 2s spawn timeout and a doomed
/// connect.
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

/// The session names already on the server, so "new" can reject a duplicate
/// and auto-suffix an omitted name.
///
/// No local server yet means no names: the auto-spawn that follows seeds
/// the chosen one. A server that does not answer contributes none either;
/// the attach that follows reports why.
fn existing_session_names(rt: &tokio::runtime::Runtime, target: &ServerTarget) -> Vec<String> {
    if target.socket_path().is_some_and(|path| !path.exists()) {
        return Vec::new();
    }
    rt.block_on(target.get_state()).map_or_else(
        |_| Vec::new(),
        // Session names are hub-local: `handle_get_state_federated`
        // discards every satellite's `sessions` list because its
        // `u32` ids would collide with the hub's. So an unreachable
        // satellite cannot hide the name we are about to reject or
        // auto-suffix, and this duplicate check is exactly as sound
        // on a degraded hub as on a healthy one. Warned, not refused.
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
        // No name given: start from the configured session-name-template,
        // the same base every auto-create path uses, and disambiguate with
        // fresh `${random-name}` picks, then a numeric suffix, instead of
        // emitting a bare "0". `${cwd-basename}` renders against the local
        // shell's directory even for a remote server: it names the session
        // after where the user typed the command.
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
/// seed pane's id as JSON.
///
/// Since the v0.3.0 "Option B" re-tier (ADR-0019 / ADR-0027) dissolved the
/// L2 collection tier and removed the `CREATE_SESSION` verb, create-without-
/// attach is expressed as an L3 `SET_METADATA` write of the conventional
/// [`SESSION_CREATE_KEY`] (`Scope::Global`, value = JSON `{name, command?,
/// cwd?}`). The server seeds the session + pane atomically; the client then
/// reads the seed-pane id back from [`SESSION_CREATE_RESULT_KEY`] via
/// `GET_METADATA` (`SET_METADATA` carries no reply frame).
///
/// `--json` requires an explicit `-s NAME` (auto-naming is reserved for the
/// attaching path); that rule is clap-enforced on the verb, so `name` is
/// already resolved here. A name already in use is reported as an error
/// (checked client-side against the pre-write snapshot) — create-only,
/// never create-or-attach.
pub(crate) fn run_new_json(
    rt: &tokio::runtime::Runtime,
    target: &ServerTarget,
    name: &str,
    cwd: Option<PathBuf>,
    command: Vec<String>,
    env: Vec<(String, String)>,
) -> ExitCode {
    // A local server must be running to host the new session. Auto-spawn
    // seeds a throwaway session under DEFAULT_SESSION_NAME (kept distinct
    // from the requested name so the create write below does not collide
    // with the seed) and keeps the server alive; the real session is then
    // created without attaching. A remote server is never spawned from here.
    // Failure is deliberately silent here, unlike the prose paths: `--json`
    // promises stderr carries the error document and nothing else, and the
    // create below fails with that document anyway. A prose line would make
    // stderr unparseable for the caller that asked for machine output.
    if let Some(path) = target.socket_path()
        && let Err(err) = ensure_server(path, DEFAULT_SESSION_NAME, None, true)
    {
        tracing::debug!(error = %err, "auto-spawn failed on the --json create path");
    }

    let cwd = seed_cwd(cwd, target.is_remote());
    let command = if command.is_empty() {
        None
    } else {
        Some(command)
    };
    let env = env.into_iter().collect();

    match rt.block_on(create_session_via_metadata(
        target, name, command, cwd, env, None, false, true,
    )) {
        Ok(terminal_id) => {
            let payload = new_session_json(name, terminal_id);
            match serde_json::to_string_pretty(&payload) {
                Ok(s) => {
                    outln!("{s}");
                    ExitCode::SUCCESS
                }
                Err(err) => {
                    eprintln!("phux: failed to serialize create result as JSON: {err}");
                    ExitCode::FAILURE
                }
            }
        }
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

async fn require_atomic_agent_session_create(
    conn: &mut Connection,
    server: &ServerTarget,
    json: bool,
) -> Result<(), ExitCode> {
    // Native restore must be atomic with session creation. Older servers treat
    // the nonce-result namespace as ordinary metadata and cannot install
    // `agent_session` in the create transaction. Current servers reserve it
    // and reject the sentinel write. This consumes no protocol capability bit.
    let probe_key = format!("{SESSION_CREATE_RESULT_KEY_PREFIX}{}", uuid::Uuid::new_v4());
    conn.send(&FrameKind::SetMetadata {
        request_id: 100,
        scope: Scope::Global,
        key: probe_key.clone(),
        value: uuid::Uuid::new_v4().as_bytes().to_vec(),
    })
    .await
    .map_err(|err| server.report_unreachable(json, &err, "new"))?;
    let (probe, interleaved) = conn
        .request_metadata(101, Scope::Global, probe_key.clone())
        .await
        .map_err(|err| server.report_unreachable(json, &err, "new"))?
        .into_parts();
    for message in phux_client::state::degradation_notices(&interleaved) {
        eprintln!("phux: warning: partial results — {message}");
    }
    match probe {
        Ok(None) => Ok(()),
        Ok(Some(_)) => {
            conn.send(&FrameKind::DeleteMetadata {
                request_id: 102,
                scope: Scope::Global,
                key: probe_key.clone(),
            })
            .await
            .map_err(|err| server.report_unreachable(json, &err, "new"))?;
            // Ordered read-back confirms that the old server processed the
            // cleanup before this connection closes.
            let _ = conn
                .request_metadata(103, Scope::Global, probe_key)
                .await
                .map_err(|err| server.report_unreachable(json, &err, "new"))?;
            eprintln!(
                "phux: create-session failed: server does not support atomic agent-session restore"
            );
            Err(ExitCode::FAILURE)
        }
        Err(refusal) => {
            eprintln!(
                "phux: create-session failed: server refused the agent-session capability probe: {refusal}"
            );
            Err(ExitCode::FAILURE)
        }
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

/// Create a named session without attaching via the conventional
/// `SESSION_CREATE_KEY` write, then read the seed-pane id back from a
/// nonce-correlated, one-shot result key.
///
/// `agent_session_preflighted` is true only when a multi-session caller has
/// already run [`preflight_atomic_agent_session_create`] before creating any
/// member of its batch; otherwise this function performs that probe itself.
/// Returns the seed pane's local id on success, or the failure `ExitCode`
/// (already reported to stderr) otherwise. Shared by `phux new --json`; mirrors
/// the MCP `phux_new` path. `server` is the local socket or a `--remote` host.
#[allow(
    clippy::too_many_arguments,
    reason = "the shared create-without-attach path keeps the complete operation explicit"
)]
pub(crate) async fn create_session_via_metadata(
    server: &ServerTarget,
    name: &str,
    command: Option<Vec<String>>,
    cwd: Option<String>,
    env: std::collections::BTreeMap<String, String>,
    agent_session: Option<Vec<u8>>,
    agent_session_preflighted: bool,
    json: bool,
) -> Result<u64, ExitCode> {
    let allow_legacy_result = agent_session.is_none();
    let request_token = uuid::Uuid::new_v4().to_string();
    let result_key = format!("{SESSION_CREATE_RESULT_KEY_PREFIX}{request_token}");
    let create_bytes = encode_create_request(
        name,
        command.as_deref(),
        cwd.as_deref(),
        &env,
        &request_token,
        agent_session.as_deref(),
    )?;

    let mut conn = server
        .connect()
        .await
        .map_err(|err| server.report_unreachable(json, &err, "new"))?;

    reject_duplicate_session_name(&mut conn, server, name, json).await?;

    if !allow_legacy_result && !agent_session_preflighted {
        require_atomic_agent_session_create(&mut conn, server, json).await?;
    }

    send_create_request(&mut conn, server, create_bytes, json).await?;

    let (bytes, correlated) = read_create_result(
        &mut conn,
        server,
        name,
        json,
        result_key,
        allow_legacy_result,
    )
    .await?;

    seed_pane_id_from_result(&bytes, name, &request_token, correlated)
        .ok_or_else(|| report_session_not_registered(name))
}

/// Encode the `SESSION_CREATE_KEY` request document.
fn encode_create_request(
    name: &str,
    command: Option<&[String]>,
    cwd: Option<&str>,
    env: &std::collections::BTreeMap<String, String>,
    request_token: &str,
    agent_session: Option<&[u8]>,
) -> Result<Vec<u8>, ExitCode> {
    serde_json::to_vec(&serde_json::json!({
        "name": name,
        "command": command,
        "cwd": cwd,
        "env": env,
        "request_token": request_token,
        "agent_session": agent_session,
    }))
    .map_err(|err| {
        eprintln!("phux: failed to serialize create request: {err}");
        ExitCode::FAILURE
    })
}

/// Reject a duplicate name before writing (the server also refuses it, but
/// silently — `SET_METADATA` has no reply frame).
///
/// Same reasoning as the human path: only `panes` aggregate across a
/// federation, so a partial fleet cannot hide a session name. The notice
/// still goes to stderr — `phux new --json` puts its *result* on stdout,
/// and a warning has no place in that document.
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
        eprintln!("phux: session '{name}' already exists");
        return Err(ExitCode::FAILURE);
    }
    Ok(())
}

/// Request the create.
///
/// Frames are ordered on the connection, while the nonce inside the request
/// prevents another concurrent creator from supplying a stale or unrelated
/// Terminal id to the read-back that follows.
async fn send_create_request(
    conn: &mut Connection,
    server: &ServerTarget,
    create_bytes: Vec<u8>,
    json: bool,
) -> Result<(), ExitCode> {
    conn.send(&FrameKind::SetMetadata {
        request_id: 1,
        scope: Scope::Global,
        key: SESSION_CREATE_KEY.to_owned(),
        value: create_bytes,
    })
    .await
    .map_err(|err| server.report_unreachable(json, &err, "new"))
}

/// Read only this request's one-shot result, falling back to the legacy
/// uncorrelated key when the caller allows it. The `bool` reports whether the
/// bytes came from the correlated key.
///
/// The read-back rides `request_metadata`, not a hand-rolled wait. The
/// loop that used to be here matched `METADATA_VALUE` on request id 2 and
/// dropped everything else, so a server that refused the read with a
/// correlated ERROR (`proto.md` §9) left `phux new` hanging with the
/// session possibly already created — the worst shape of this bug, because
/// the user's Ctrl-C then looks like the create failed.
async fn read_create_result(
    conn: &mut Connection,
    server: &ServerTarget,
    name: &str,
    json: bool,
    result_key: String,
    allow_legacy_result: bool,
) -> Result<(Vec<u8>, bool), ExitCode> {
    let (answer, interleaved) = conn
        .request_metadata(2, Scope::Global, result_key)
        .await
        .map_err(|err| server.report_unreachable(json, &err, "new"))?
        .into_parts();
    warn_partial_results(&interleaved);
    let result_value = answer.map_err(|refusal| {
        // Distinct from "no value": the server declined to answer at all, so
        // saying "did not register" below would be a guess stated as a fact.
        eprintln!("phux: create-session failed: server refused the read-back: {refusal}");
        ExitCode::FAILURE
    })?;
    if let Some(bytes) = result_value {
        return Ok((bytes, true));
    }
    if !allow_legacy_result {
        return Err(report_session_not_registered(name));
    }
    let legacy = read_legacy_create_result(conn, server, name, json).await?;
    Ok((legacy, false))
}

/// Read the uncorrelated `SESSION_CREATE_RESULT_KEY` a server that predates
/// the nonce still answers on.
async fn read_legacy_create_result(
    conn: &mut Connection,
    server: &ServerTarget,
    name: &str,
    json: bool,
) -> Result<Vec<u8>, ExitCode> {
    let (legacy_answer, legacy_interleaved) = conn
        .request_metadata(3, Scope::Global, SESSION_CREATE_RESULT_KEY.to_owned())
        .await
        .map_err(|err| server.report_unreachable(json, &err, "new"))?
        .into_parts();
    warn_partial_results(&legacy_interleaved);
    legacy_answer
        .map_err(|refusal| {
            eprintln!("phux: create-session failed: server refused legacy read-back: {refusal}");
            ExitCode::FAILURE
        })?
        .ok_or_else(|| report_session_not_registered(name))
}

/// Report every degradation notice that rode along with a metadata answer.
fn warn_partial_results(interleaved: &[FrameKind]) {
    for message in phux_client::state::degradation_notices(interleaved) {
        eprintln!("phux: warning: partial results — {message}");
    }
}

/// The failure reported when no create result names the requested session.
fn report_session_not_registered(name: &str) -> ExitCode {
    eprintln!("phux: create-session failed: server did not register session '{name}'");
    ExitCode::FAILURE
}

/// Read the seed pane's Terminal id out of a create-result document, rejecting
/// one that does not answer for this request: the name must match, and the
/// nonce must be present on a correlated read and absent on a legacy one.
fn seed_pane_id_from_result(
    bytes: &[u8],
    name: &str,
    request_token: &str,
    correlated: bool,
) -> Option<u64> {
    serde_json::from_slice::<serde_json::Value>(bytes)
        .ok()
        .filter(|v| v.get("name").and_then(serde_json::Value::as_str) == Some(name))
        .filter(|v| {
            if correlated {
                v.get("request_token").and_then(serde_json::Value::as_str) == Some(request_token)
            } else {
                v.get("request_token").is_none()
            }
        })
        .and_then(|v| v.get("terminal_id").and_then(serde_json::Value::as_u64))
}

/// The seed pane's working directory (phux-0db).
///
/// An explicit `--cwd` wins. An omitted one defaults to the *client's* cwd
/// for a local server instead of `None`: `cwd: None` on the wire makes the
/// seed pane inherit the daemon's CWD (typically `$HOME` for a long-lived
/// server), which breaks tools whose persistence is keyed by directory —
/// the `claude --resume` bug. A remote server gets no default, because a
/// path on this machine names nothing on that one; it starts the pane in
/// its own default directory instead.
fn seed_cwd(explicit: Option<PathBuf>, remote: bool) -> Option<String> {
    let explicit = explicit.map(|path| path.to_string_lossy().into_owned());
    if remote {
        return explicit;
    }
    explicit.or_else(client_cwd)
}

/// Build the `CreateIfMissing` target for `phux new` from an already
/// resolved seed cwd (see [`seed_cwd`]).
///
/// The server validates the path and falls back to its default spawn
/// directory when it is not an enterable directory on the server host, so a
/// stale or foreign client path can never fail the create.
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

/// A session name for an omitted-name `phux new` that is not in `existing`.
///
/// Renders `template` once; a free result wins. A taken result from a
/// `${random-name}` template is re-rendered with fresh picks up to
/// [`RANDOM_NAME_ATTEMPTS`] times in all. When every pick is taken, or the
/// template is deterministic, the first render gets the numeric suffix
/// [`unique_session_name`] guarantees.
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

/// `base` if it is free, otherwise `base-2`, `base-3`, … — the first
/// available name. Lets `phux new` (no name given) reuse the configured
/// session-name-template as its base and still guarantee a distinct
/// session each time, instead of emitting bare numeric names ("0", "1").
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
        AttachTarget, Path, PathBuf, RANDOM_NAME_ATTEMPTS, choose_session_name, fresh_session_name,
        new_session_json, new_session_target, requested_session_name, seed_cwd,
        unique_session_name,
    };
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
