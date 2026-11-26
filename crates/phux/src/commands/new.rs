use std::path::PathBuf;
use std::process::ExitCode;

use phux_protocol::ids::IdempotencyKey;
use phux_protocol::wire::frame::SessionLifetime;

use crate::commands::attach::{
    client_cwd, interactive_tty_preflight, report_attach_end, run_attach_once,
};
use crate::commands::server_target::{ServerSpec, ServerTarget};

mod create;
mod empty;
mod naming;
/// Run `phux new` as parsed.
pub(crate) fn run(args: crate::commands::NewArgs, socket: Option<std::path::PathBuf>) -> ExitCode {
    let crate::commands::NewArgs {
        name,
        session,
        cwd,
        json,
        env,
        idempotency_key,
        remote,
        empty,
        ephemeral,
        command,
    } = args;
    match crate::commands::spawn::parse_key_arg(idempotency_key.as_deref(), json) {
        Ok(idempotency_key) => run_new(
            name,
            session,
            cwd,
            remote.with_socket(socket),
            NewMode {
                json,
                empty,
                ephemeral,
                idempotency_key,
            },
            command,
            env.into_iter().map(|item| (item.key, item.value)).collect(),
        ),
        Err(code) => code,
    }
}

#[cfg(test)]
mod tests;
#[cfg(test)]
pub(crate) mod workflow_tests;

pub(crate) use create::{
    committed_session_error, create_seeded_session, report_create_session_error,
};
pub(crate) use empty::ensure_local_unseeded_server;
use empty::{run_new_empty_attached, run_new_empty_json};
pub(crate) use naming::{existing_session_names, unique_session_name};
use naming::{report_invalid_session_name, requested_session_name, resolve_session_name};

/// `phux new` — create a *new* session and attach to it.
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
        ephemeral,
        idempotency_key,
    } = mode;
    let requested = match requested_session_name(name, session) {
        Ok(requested) => requested,
        Err(code) => return code,
    };
    let lifetime = session_lifetime(empty, ephemeral);
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
        return run_new_attached(&rt, &target, requested, command, cwd, lifetime);
    }
    // Clap enforces `--json` => `-s NAME`; this guard only keeps a future caller
    // from panicking.
    let Some(name) = requested else {
        error!("`phux new --json` requires an explicit -s NAME");
        return ExitCode::from(2);
    };
    if empty {
        return run_new_empty_json(&rt, &target, &name);
    }
    run_new_json(
        &rt,
        &target,
        &name,
        cwd,
        command,
        env,
        idempotency_key,
        lifetime,
    )
}

/// The lifetime `phux new` asks for: a session someone creates is
/// persistent, named or not, unless `--ephemeral` asks otherwise. An empty
/// session is always persistent, since an ephemeral session is never empty.
pub(crate) const fn session_lifetime(empty: bool, ephemeral: bool) -> SessionLifetime {
    if empty || !ephemeral {
        SessionLifetime::Persistent
    } else {
        SessionLifetime::Ephemeral
    }
}

/// How `phux new` runs: headless (`--json`), whether the session starts
/// with no terminal (`--empty`), its lifetime override (`--ephemeral`), and
/// whether the create is keyed (`--idempotency-key`, ADR-0126).
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct NewMode {
    /// `--json`: create without attaching and print a document.
    pub(crate) json: bool,
    /// `--empty`: create a persistent session with no process.
    pub(crate) empty: bool,
    /// `--ephemeral`: the session ends with its last process.
    pub(crate) ephemeral: bool,
    /// `--idempotency-key`, already parsed: the create's `request_token`.
    pub(crate) idempotency_key: Option<IdempotencyKey>,
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
    lifetime: SessionLifetime,
) -> ExitCode {
    if let Some(path) = target.socket_path()
        && let Err(code) = super::attach::nested_attach_guard(path)
    {
        return code;
    }
    let name = resolve_session_name(rt, target, requested);

    ensure_local_unseeded_server(target, false);
    let session_target = match rt.block_on(create::create_attached_session(
        target,
        &name,
        command,
        seed_cwd(cwd, target.is_remote()),
        lifetime,
    )) {
        Ok(target) => target,
        Err(code) => return code,
    };

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
            warn!("session '{name}' was created; inspect `phux ls` before retrying the attach");
            target.report_attach_failure(&err, &name);
            ExitCode::FAILURE
        }
    }
}

/// `phux new --json` creates without attaching. The host's `SESSION_CREATE`
/// refuses a name collision atomically; an uncertain receipt never permits a
/// fresh automatic create. Requires `-s NAME` (clap-enforced).
#[allow(
    clippy::too_many_arguments,
    reason = "one argument per `phux new` input; bundling them hides which the JSON path reads"
)]
pub(crate) fn run_new_json(
    rt: &tokio::runtime::Runtime,
    target: &ServerTarget,
    name: &str,
    cwd: Option<PathBuf>,
    command: Vec<String>,
    env: Vec<(String, String)>,
    idempotency_key: Option<IdempotencyKey>,
    lifetime: SessionLifetime,
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

    match rt.block_on(create_seeded_session(
        target,
        name,
        command,
        cwd,
        env,
        None,
        true,
        idempotency_key,
        lifetime,
        &[],
    )) {
        Ok(terminal_id) => crate::output::json(&new_session_json(name, terminal_id)),
        Err(code) => code,
    }
}

/// The `phux new --json` result document: the created session's name and its
/// seed pane's wire-local id. Pure, so the shape (including `schema_version`)
/// is unit-testable without a server. `phux ls --json` reports the lifetime.
fn new_session_json(session: &str, terminal_id: u64) -> serde_json::Value {
    serde_json::json!({
        "schema_version": 1,
        "session": session,
        "terminal_id": terminal_id,
    })
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
