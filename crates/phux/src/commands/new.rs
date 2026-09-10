use std::path::{Path, PathBuf};
use std::process::ExitCode;

use phux_client::attach::Dial;
use phux_client::attach::connection::Connection;
use phux_protocol::wire::frame::{
    AttachTarget, FrameKind, SESSION_CREATE_KEY, SESSION_CREATE_RESULT_KEY,
    SESSION_CREATE_RESULT_KEY_PREFIX, Scope,
};
use phux_server::runtime::default_socket_path;

use crate::commands::{
    DEFAULT_SESSION_NAME, attach::client_cwd, attach::configured_session_name_template,
    attach::interactive_tty_preflight, attach::render_default_session_name,
    attach::report_attach_end, attach::run_attach_once, cli_runtime, json_err, partial,
    print_attach_error, server::ensure_server,
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
/// falls back to the configured `session-name-template` (e.g. "default"),
/// disambiguated with a numeric suffix if taken. The create+attach itself
/// rides `CreateIfMissing` (ADR-0021 defers a dedicated create-session
/// command).
pub(crate) fn run_new(
    name: Option<String>,
    session: Option<String>,
    cwd: Option<PathBuf>,
    socket: Option<PathBuf>,
    json: bool,
    command: Vec<String>,
    env: Vec<(String, String)>,
) -> ExitCode {
    // The session name can come from the positional NAME or the `-s` flag;
    // they are the same field with two spellings. Reject a genuine conflict
    // rather than silently picking one.
    let requested = match (name, session) {
        (Some(positional), Some(flag)) if positional != flag => {
            eprintln!(
                "phux: conflicting session names: '{positional}' (positional) vs '{flag}' (-s) — pass just one"
            );
            return ExitCode::FAILURE;
        }
        (Some(positional), _) => Some(positional),
        (None, flag) => flag,
    };

    if !json && let Err(code) = interactive_tty_preflight() {
        return code;
    }

    let socket_path = socket.unwrap_or_else(default_socket_path);
    // phux-iwuc: fail before auto-spawn with the sockaddr_un limit named,
    // instead of the 2s spawn timeout + a doomed connect.
    if let Err(code) = crate::commands::ensure_socket_path_fits(&socket_path) {
        return code;
    }
    let rt = match cli_runtime() {
        Ok(rt) => rt,
        Err(code) => return code,
    };

    if json {
        // The `--json` ⇒ `-s NAME` rule is clap-enforced on the `new` verb
        // (phux-i0e8.8.4), so the CLI cannot reach this guard; it exists
        // only so a future non-clap caller fails with the same exit code
        // clap's usage error carries, never a panic.
        let Some(name) = requested else {
            eprintln!("phux: `phux new --json` requires an explicit -s NAME");
            return ExitCode::from(2);
        };
        return run_new_json(&rt, &socket_path, &name, cwd, command, env);
    }

    // If a server is up, snapshot its session names so we can enforce
    // "new" (reject a duplicate -s, auto-name an omitted one). No server
    // yet → no existing names; the auto-spawn below seeds the chosen name.
    let existing = if socket_path.exists() {
        rt.block_on(phux_client::state::get_state(&socket_path))
            .map_or_else(
                // A server that did not answer contributes no names.
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
    } else {
        Vec::new()
    };

    let name = match requested {
        Some(requested) => {
            if existing.contains(&requested) {
                eprintln!(
                    "phux: session '{requested}' already exists (use `phux attach {requested}` to join it)"
                );
                return ExitCode::FAILURE;
            }
            requested
        }
        // No name given: start from the configured session-name-template
        // (e.g. "default"), the same base every auto-create path uses, and
        // disambiguate with fresh `${random-name}` picks, then a numeric
        // suffix, instead of emitting a bare "0".
        None => fresh_session_name(
            &existing,
            &configured_session_name_template(),
            &std::env::current_dir().unwrap_or_default(),
            &mut phux_config::NameRng::from_entropy(),
        ),
    };

    // phux-07y: `phux new` never seeds with spawn-on-attach — an
    // explicitly-created session gets a plain shell (or the `-- CMD`
    // the user gave, applied per-session via CreateIfMissing).
    if let Err(err) = ensure_server(&socket_path, &name, None, json) {
        eprintln!("phux: auto-spawn skipped ({err}). Start a server manually with `phux server`.");
    }

    let target = new_session_target(name.clone(), command, cwd);

    // `phux new` is a local-socket verb by construction, so the shared
    // resolver settles on the UDS answer: prediction off unless the config
    // asks for it explicitly.
    let dial = Dial::uds(&socket_path);
    let predict_cfg = super::attach::predictive_config_for(&dial);
    match rt.block_on(run_attach_once(&dial, target, predict_cfg)) {
        Ok(attach_end) => {
            // phux-i0e8.2.2: same one-line ending explanation as `phux
            // attach` — a last-pane death is named, a detach stays quiet.
            report_attach_end(attach_end);
            ExitCode::SUCCESS
        }
        Err(err) => {
            print_attach_error(&err, &socket_path, &name);
            ExitCode::FAILURE
        }
    }
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
    socket_path: &Path,
    name: &str,
    cwd: Option<PathBuf>,
    command: Vec<String>,
    env: Vec<(String, String)>,
) -> ExitCode {
    // A server must be running to host the new session. Auto-spawn seeds a
    // throwaway session under DEFAULT_SESSION_NAME (kept distinct from the
    // requested name so the create write below does not collide with the
    // seed) and keeps the server alive; the real session is then created
    // without attaching.
    // Failure is deliberately silent here, unlike the prose paths: `--json`
    // promises stderr carries the error document and nothing else, and the
    // create below fails with that document anyway. A prose line would make
    // stderr unparseable for the caller that asked for machine output.
    if let Err(err) = ensure_server(socket_path, DEFAULT_SESSION_NAME, None, true) {
        tracing::debug!(error = %err, "auto-spawn failed on the --json create path");
    }

    // phux-0db: like the attaching path, an omitted `--cwd` defaults to
    // the client's cwd rather than `None` (= the daemon's CWD).
    let cwd = cwd
        .map(|p| p.to_string_lossy().into_owned())
        .or_else(client_cwd);
    let command = if command.is_empty() {
        None
    } else {
        Some(command)
    };
    let env = env.into_iter().collect();

    match rt.block_on(create_session_via_metadata(
        socket_path,
        name,
        command,
        cwd,
        env,
        None,
        false,
        true,
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
    socket_path: &Path,
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
    .map_err(|err| json_err::report_no_server(json, &err, socket_path, "new"))?;
    let (probe, interleaved) = conn
        .request_metadata(101, Scope::Global, probe_key.clone())
        .await
        .map_err(|err| json_err::report_no_server(json, &err, socket_path, "new"))?
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
            .map_err(|err| json_err::report_no_server(json, &err, socket_path, "new"))?;
            // Ordered read-back confirms that the old server processed the
            // cleanup before this connection closes.
            let _ = conn
                .request_metadata(103, Scope::Global, probe_key)
                .await
                .map_err(|err| json_err::report_no_server(json, &err, socket_path, "new"))?;
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
    let mut conn = Connection::connect(socket_path)
        .await
        .map_err(|err| crate::commands::report_no_server(&err, socket_path, "workspace restore"))?;
    require_atomic_agent_session_create(&mut conn, socket_path, false).await
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
/// the MCP `phux_new` path.
#[allow(
    clippy::too_many_arguments,
    reason = "the shared create-without-attach path keeps the complete operation explicit"
)]
pub(crate) async fn create_session_via_metadata(
    socket_path: &Path,
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

    let mut conn = Connection::connect(socket_path)
        .await
        .map_err(|err| json_err::report_no_server(json, &err, socket_path, "new"))?;

    reject_duplicate_session_name(&mut conn, socket_path, name, json).await?;

    if !allow_legacy_result && !agent_session_preflighted {
        require_atomic_agent_session_create(&mut conn, socket_path, json).await?;
    }

    send_create_request(&mut conn, socket_path, create_bytes, json).await?;

    let (bytes, correlated) = read_create_result(
        &mut conn,
        socket_path,
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
    socket_path: &Path,
    name: &str,
    json: bool,
) -> Result<(), ExitCode> {
    let (pre, degradation) = phux_client::state::get_state_on(conn)
        .await
        .map_err(|err| json_err::report_no_server(json, &err, socket_path, "new"))?
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
    socket_path: &Path,
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
    .map_err(|err| json_err::report_no_server(json, &err, socket_path, "new"))
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
    socket_path: &Path,
    name: &str,
    json: bool,
    result_key: String,
    allow_legacy_result: bool,
) -> Result<(Vec<u8>, bool), ExitCode> {
    let (answer, interleaved) = conn
        .request_metadata(2, Scope::Global, result_key)
        .await
        .map_err(|err| json_err::report_no_server(json, &err, socket_path, "new"))?
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
    let legacy = read_legacy_create_result(conn, socket_path, name, json).await?;
    Ok((legacy, false))
}

/// Read the uncorrelated `SESSION_CREATE_RESULT_KEY` a server that predates
/// the nonce still answers on.
async fn read_legacy_create_result(
    conn: &mut Connection,
    socket_path: &Path,
    name: &str,
    json: bool,
) -> Result<Vec<u8>, ExitCode> {
    let (legacy_answer, legacy_interleaved) = conn
        .request_metadata(3, Scope::Global, SESSION_CREATE_RESULT_KEY.to_owned())
        .await
        .map_err(|err| json_err::report_no_server(json, &err, socket_path, "new"))?
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

/// Build the `CreateIfMissing` target for `phux new` (phux-0db).
///
/// An explicit `--cwd` wins; an omitted one defaults to the *client's*
/// current working directory instead of `None`. `cwd: None` on the wire
/// makes the seed pane inherit the daemon's CWD (typically `$HOME` for a
/// long-lived server), which breaks tools whose persistence is keyed by
/// directory — the `claude --resume` bug. The server validates the path
/// and falls back to its default spawn directory when it is not an
/// enterable directory on the server host, so a stale or foreign client
/// path can never fail the create.
fn new_session_target(name: String, command: Vec<String>, cwd: Option<PathBuf>) -> AttachTarget {
    AttachTarget::CreateIfMissing {
        name,
        command: if command.is_empty() {
            None
        } else {
            Some(command)
        },
        cwd: cwd
            .map(|p| p.to_string_lossy().into_owned())
            .or_else(client_cwd),
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
        AttachTarget, Path, PathBuf, RANDOM_NAME_ATTEMPTS, fresh_session_name, new_session_json,
        new_session_target, unique_session_name,
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

    /// phux-0db: `phux new` without `--cwd` seeds the session in the
    /// *client's* cwd, not `None` (= the daemon's CWD).
    #[test]
    fn new_session_target_defaults_cwd_to_client_cwd() {
        let expected = std::env::current_dir()
            .expect("test cwd")
            .to_string_lossy()
            .into_owned();
        assert_eq!(
            new_session_target("proj".to_owned(), Vec::new(), None),
            AttachTarget::CreateIfMissing {
                name: "proj".to_owned(),
                command: None,
                cwd: Some(expected),
            }
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
                Some(PathBuf::from("/somewhere/else")),
            ),
            AttachTarget::CreateIfMissing {
                name: "proj".to_owned(),
                command: Some(vec!["vim".to_owned(), "notes.txt".to_owned()]),
                cwd: Some("/somewhere/else".to_owned()),
            }
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
