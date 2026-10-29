use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::ExitCode;

use phux_protocol::ids::IdempotencyKey;
use phux_protocol::wire::frame::{AttachTarget, SessionLifetime};

use crate::commands::server_target::{ServerSpec, ServerTarget};
use crate::commands::{
    attach::client_cwd, attach::interactive_tty_preflight, attach::report_attach_end,
    attach::run_attach_once, server::ensure_server,
};

mod create;
mod empty;
mod naming;
#[cfg(test)]
mod tests;

pub(crate) use create::create_session_via_metadata;
pub(crate) use empty::ensure_local_unseeded_server;
use empty::{run_new_empty_attached, run_new_empty_json};
use naming::{
    choose_session_name, existing_session_names, report_invalid_session_name,
    requested_session_name,
};

/// `phux new` — create a *new* session and attach to it.
///
/// The name comes from the positional `NAME` or `-s` (a conflict is an error).
/// A name that already exists is refused; an omitted one renders
/// `session-name-template`, redrawing a taken `${random-name}` and settling on a
/// numeric suffix. Only a local server is auto-spawned.
///
/// Lifetime follows [`session_lifetime`]: a named session is persistent and
/// is created headless, then attached by name; a generated name, or
/// `--ephemeral`, makes it ephemeral, and the create+attach rides
/// `CreateIfMissing`. `mode.empty` creates a persistent session with no
/// terminal.
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
    let lifetime = session_lifetime(requested.is_some(), empty, ephemeral);
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

/// The lifetime `phux new` asks for (ADR-0160): an empty session is always
/// persistent (an ephemeral session is never empty); otherwise
/// `--ephemeral` wins, and a session the user named is persistent while one
/// whose name was generated is ephemeral.
pub(crate) const fn session_lifetime(named: bool, empty: bool, ephemeral: bool) -> SessionLifetime {
    if empty || (named && !ephemeral) {
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
///
/// A persistent session is created headless with its lifetime
/// ([`create_session_via_metadata`]) and then attached by name, because an
/// attach-time `CreateIfMissing` carries no lifetime and the server makes
/// such a session ephemeral. An ephemeral one rides `CreateIfMissing`.
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
    let existing = existing_session_names(rt, target);
    let name = match choose_session_name(requested, &existing) {
        Ok(name) => name,
        Err(code) => return code,
    };

    let session_target = if lifetime == SessionLifetime::Persistent {
        ensure_local_unseeded_server(target, false);
        let command = if command.is_empty() {
            None
        } else {
            Some(command)
        };
        if let Err(code) = rt.block_on(create_session_via_metadata(
            target,
            &name,
            command,
            seed_cwd(cwd, target.is_remote()),
            BTreeMap::new(),
            None,
            false,
            None,
            lifetime,
            &[],
        )) {
            return code;
        }
        AttachTarget::ByName(name.clone())
    } else {
        // phux-07y: `phux new` never seeds with spawn-on-attach — an
        // explicitly-created session gets a plain shell (or the `-- CMD`
        // the user gave, applied per-session via CreateIfMissing).
        if let Some(path) = target.socket_path()
            && let Err(err) = ensure_server(path, &name, None, false)
        {
            warn!("auto-spawn skipped ({err}). Start a server manually with `phux server`.");
        }
        new_session_target(name.clone(), command, seed_cwd(cwd, target.is_remote()))
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
            target.report_attach_failure(&err, &name);
            ExitCode::FAILURE
        }
    }
}

/// `phux new --json` — create a session *without* attaching and print its
/// seed pane's id, via [`phux_client::session::create_session`]: one
/// `SESSION_CREATE` carrying `lifetime` against a server that advertises
/// `ORGANIZATION`, else the `phux.session.create/v1` write and its
/// nonce-correlated read-back. Requires `-s NAME` (clap-enforced); a name in
/// use is an error.
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

    match rt.block_on(create_session_via_metadata(
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
