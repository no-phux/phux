//! Local attach entry points and lookup-only diagnostics.
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use phux_client::attach::{AttachError, Dial};
use phux_config::socket::default_socket_path;

use crate::commands::rec::RecordSpec;
use crate::commands::remote as host_registry;
use crate::commands::{print_attach_error, server::ensure_server};
use super::home::{
    configured_spawn_on_attach, fallback_lookup_name, requested_attach_target,
    resolved_default_session_name,
};
use super::{
    attach_with_reconnect, finalize_recording, home, interactive_tty_preflight,
    nested_attach_guard, predictive_config_for, report_attach_end,
};

/// `phux attach [NAME]` without recording.
pub(crate) fn run_attach(session: Option<String>, socket: Option<PathBuf>) -> ExitCode {
    run_attach_rec(session, socket, None)
}

/// Run the attach loop on a current-thread runtime and map the result to an
/// exit code, auto-spawning `phux server` first if nothing accepts (see
/// [`ensure_server`]).
pub(crate) fn run_attach_rec(
    session: Option<String>,
    socket: Option<PathBuf>,
    rec: Option<&RecordSpec>,
) -> ExitCode {
    if let Err(code) = interactive_tty_preflight() {
        return code;
    }

    // A registered host name wins over a local session of the same name, unless
    // `--socket` says local; it goes through the same repair ladder as `--remote`.
    if socket.is_none()
        && let Some(name) = session.as_deref()
        && let Some(entry) = host_registry::find(name)
    {
        return crate::commands::remote_target::run_registered(name, entry, rec);
    }

    let socket_path = socket.unwrap_or_else(default_socket_path);
    // phux-iwuc: fail before auto-spawn with the sockaddr_un limit named,
    // instead of the 2s spawn timeout + a doomed connect.
    if let Err(code) = crate::commands::ensure_socket_path_fits(&socket_path) {
        return code;
    }
    if let Err(code) = nested_attach_guard(&socket_path) {
        return code;
    }
    // Resolve only the auto-spawn seed before moving `session` into the wire
    // target. With no name this is `defaults.attach-session`, else the
    // configured template; the server owns that seed and resolves `Last`.
    let home = session.is_none().then(home::configured_session).flatten();
    let default_name = home
        .as_ref()
        .map_or_else(resolved_default_session_name, |home| home.name.clone());
    let session_for_spawn = session.clone().unwrap_or_else(|| default_name.clone());
    let explicit_name = session.clone();
    // Only a naked/default Last attach may bootstrap the server's default seed.
    // Explicit selection never creates; configured creation is the host ensure
    // below, so an older peer cannot accept a hidden seed first.
    let started = if home.is_some() || session.is_some() {
        crate::commands::server::ensure_server_unseeded(&socket_path, false)
    } else {
        ensure_server(
            &socket_path, &session_for_spawn,
            configured_spawn_on_attach().as_deref(), false,
        )
    };
    let startup = started.as_ref().ok().copied();
    if let Err(err) = started {
        warn!("auto-spawn skipped ({err}). Start a server manually with `phux server`.");
    }

    let rt = match crate::commands::cli_runtime() {
        Ok(rt) => rt,
        Err(code) => return code,
    };

    let dial = Dial::uds(&socket_path);
    let predict_cfg = predictive_config_for(&dial);
    let (first, target, default) = match home {
        Some(home) => match rt.block_on(home.ensure_target(&dial, startup)) {
            Ok((connection, target)) => (Some(Box::new(connection)), target, None),
            Err(err) => {
                error!("could not prepare configured session '{default_name}': {err}");
                if matches!(err, AttachError::CommittedSelectionUnavailable { .. }) {
                    hint!("The session operation committed. Recover its exact key; do not create it again.");
                } else {
                    hint!("Inspect `phux ls` before retrying; an uncertain ensure may have committed.");
                }
                return ExitCode::FAILURE;
            }
        },
        None => {
            let default = session.is_none().then(|| fallback_lookup_name(&default_name)).flatten();
            (None, requested_attach_target(session), default)
        }
    };
    let result = rt.block_on(attach_with_reconnect(
        &dial, first, target, predict_cfg, default, rec,
    ));
    finalize_recording(rec);
    let exit = match result {
        Ok(end) => {
            report_attach_end(end);
            ExitCode::SUCCESS
        }
        // Already reported by the reconnect window (distinct SocketGone /
        // TimedOut lines naming `phux doctor`) — see `attach_with_reconnect`.
        Err(AttachError::Disconnected) => ExitCode::FAILURE,
        // Attach never creates: a name the live server does not hold names
        // the verb that does, instead of only relaying the refusal.
        Err(AttachError::Refused(_))
            if explicit_name.as_deref().is_some_and(|name| {
                rt.block_on(phux_client::state::get_state(&socket_path))
                    .is_ok_and(|view| session_is_missing(view.snapshot(), name))
            }) =>
        {
            crate::output::error_block(
                missing_session_lines(explicit_name.as_deref().unwrap_or_default())
                    .iter()
                    .map(String::as_str),
            );
            ExitCode::FAILURE
        }
        Err(err) => {
            // `phux-roz` (5): produce actionable text per variant. The
            // guard (if any) has already dropped, so this lands on the
            // cooked terminal.
            print_attach_error(&err, &socket_path, &session_for_spawn);
            ExitCode::FAILURE
        }
    };
    // `tokio::io::stdin` delegates reads to a blocking-pool task. An attach
    // whose transport disappears while stdin is idle has no way to cancel
    // that OS read; dropping the runtime would therefore wait forever and
    // leave the CLI stuck after it has already restored the terminal.
    rt.shutdown_timeout(Duration::ZERO);
    exit
}

/// Whether `snapshot` (a complete `GET_STATE`: session lists never
/// aggregate satellites) holds no session named `name`.
pub(super) fn session_is_missing(snapshot: &phux_protocol::wire::info::SessionSnapshot, name: &str) -> bool {
    snapshot.sessions.iter().all(|session| session.name != name)
}

/// The refusal for `phux attach NAME` when the server has no `NAME`: attach
/// is lookup-only, so the remedy is the verb that creates one.
pub(super) fn missing_session_lines(name: &str) -> Vec<String> {
    vec![
        format!("no session named {name:?} on this server"),
        format!(
            "create it with `phux new {}`, or run `phux ls` to list sessions",
            crate::commands::ssh_bootstrap::shell_quote(name)
        ),
    ]
}

/// Naked `phux` shares the default attach policy and reconnect pacing.
pub(crate) fn run_naked(socket: Option<PathBuf>, rec: Option<&RecordSpec>) -> ExitCode {
    run_attach_rec(None, socket, rec)
}
