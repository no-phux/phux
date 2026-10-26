//! `phux doctor` — one command that answers "why isn't this working?", by
//! composing the checks behind `config check`, `plugin validate`, the socket
//! length guard, a `GET_STATE` probe, and the log inventory.
//!
//! Two rules: a check that cannot run reports [`Status::Warn`], never `Pass`;
//! and nothing here mutates anything.

use std::path::PathBuf;
use std::process::ExitCode;

use phux_server::runtime::default_socket_path;

use crate::commands::{cli_runtime, plugin::valid_manifest_count};

mod credentials;
mod remote;
mod remote_cert;
mod server_health;

use credentials::{check_client_certs, check_token_store, check_workload_authority};
use remote::{check_remote_listeners, check_remote_reachable};
use remote_cert::{check_remote_cert, check_remote_cert_chain};
use server_health::check_server_health;

/// The outcome of one check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Status {
    /// Verified working.
    Pass,
    /// Could not be verified, or is inapplicable right now. Not a failure,
    /// and deliberately not a pass either.
    Warn,
    /// Verified broken.
    Fail,
}

impl Status {
    const fn severity(self) -> crate::output::Severity {
        match self {
            Self::Pass => crate::output::Severity::Ok,
            Self::Warn => crate::output::Severity::Warn,
            Self::Fail => crate::output::Severity::Error,
        }
    }

    const fn as_str(self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Warn => "warn",
            Self::Fail => "fail",
        }
    }
}

/// One line of the report.
#[derive(Debug, Clone)]
pub(crate) struct Check {
    /// Short stable identifier, usable as a grep target and a JSON key.
    pub(crate) name: &'static str,
    pub(crate) status: Status,
    /// One line: what was found, and where.
    pub(crate) detail: String,
    /// What to do about it. Only set when there is something to do.
    pub(crate) hint: Option<String>,
}

impl Check {
    fn pass(name: &'static str, detail: impl Into<String>) -> Self {
        Self {
            name,
            status: Status::Pass,
            detail: detail.into(),
            hint: None,
        }
    }

    fn warn(name: &'static str, detail: impl Into<String>, hint: impl Into<String>) -> Self {
        Self {
            name,
            status: Status::Warn,
            detail: detail.into(),
            hint: Some(hint.into()),
        }
    }

    fn fail(name: &'static str, detail: impl Into<String>, hint: impl Into<String>) -> Self {
        Self {
            name,
            status: Status::Fail,
            detail: detail.into(),
            hint: Some(hint.into()),
        }
    }
}

/// Exit codes: 0 when nothing failed (warnings do not fail the run), 1 when
/// any check failed.
pub(crate) fn run_doctor(json: bool, socket: Option<PathBuf>) -> ExitCode {
    let socket_path = socket.unwrap_or_else(default_socket_path);
    let mut checks = vec![
        check_config(),
        check_instance(),
        check_socket_path(&socket_path),
        check_server(&socket_path),
    ];
    // Several server-health conditions can hold at once; each is reported.
    checks.extend(check_server_health(&socket_path));
    checks.extend([
        check_plugins(),
        check_agent_shim(),
        check_remote_cert(),
        check_token_store(),
        check_workload_authority(),
        check_client_certs(),
        check_remote_listeners(&socket_path),
        check_remote_reachable(&socket_path),
        check_logs(),
    ]);
    checks.extend(check_remote_cert_chain());

    if json {
        return report_json(&checks);
    }
    report_human(&checks)
}

// ---------------------------------------------------------------------------
// checks
// ---------------------------------------------------------------------------

/// Does the config parse, and does every key exist in the schema? Reuses
/// `phux config check`.
fn check_config() -> Check {
    let path = phux_config::loader::config_path();

    let body = match std::fs::read_to_string(&path) {
        Ok(body) => body,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Check::pass(
                "config",
                format!("no config at {} (shipped defaults apply)", path.display()),
            );
        }
        Err(err) => {
            return Check::fail(
                "config",
                format!("cannot read {}: {err}", path.display()),
                "fix the file's permissions, or point XDG_CONFIG_HOME elsewhere",
            );
        }
    };

    match phux_config::check::check(&body, &path) {
        Ok(report) if report.is_ok() => {
            Check::pass("config", format!("{} is valid", path.display()))
        }
        Ok(report) => {
            let n = report.findings.len();
            let plural = if n == 1 { "problem" } else { "problems" };
            Check::fail(
                "config",
                format!("{n} {plural} in {}", path.display()),
                "run `phux config check` for the full list with key paths",
            )
        }
        Err(err) => Check::fail(
            "config",
            format!("{err}"),
            "run `phux config check` for the parse position",
        ),
    }
}

/// Which instance is this binary talking to, and why? Profile isolation is
/// automatic and silent, so naming it answers "my sessions are gone".
fn check_instance() -> Check {
    let profile = phux_config::instance::profile();
    let state = phux_config::instance::state_dir();
    if phux_config::instance::is_default_profile() {
        return Check::pass(
            "instance",
            format!("profile {profile}; state {}", state.display()),
        );
    }
    let reason = if std::env::var_os("PHUX_PROFILE").is_some() {
        "PHUX_PROFILE is set"
    } else {
        "this is a development build (not an installed release)"
    };
    Check::warn(
        "instance",
        format!("profile {profile} ({reason}); state {}", state.display()),
        "this instance is isolated from your installed phux — its sessions and \
         logs are separate. Unset PHUX_PROFILE, or run the installed binary, to \
         reach the default instance",
    )
}

/// Will the socket path fit in a `sockaddr_un`? Otherwise connects time out
/// with no explanation.
fn check_socket_path(socket_path: &std::path::Path) -> Check {
    match phux_server::runtime::validate_socket_path_len(socket_path) {
        Ok(()) => Check::pass("socket-path", socket_path.display().to_string()),
        Err(err) => Check::fail(
            "socket-path",
            err.to_string(),
            "set PHUX_SOCKET (or --socket) to a shorter path, e.g. under /tmp",
        ),
    }
}

/// Is a server running, and does it speak a protocol this binary knows? A
/// stopped server is a warning: running doctor first is ordinary.
pub(super) fn check_server(socket_path: &std::path::Path) -> Check {
    if !socket_path.exists() {
        return Check::warn(
            "server",
            format!("no server at {}", socket_path.display()),
            "start one with `phux` (auto-spawns) or `phux server`",
        );
    }

    let Ok(rt) = cli_runtime() else {
        return Check::warn(
            "server",
            "could not build a runtime to probe the server",
            "retry; if this persists it is a bug worth filing",
        );
    };

    match rt.block_on(phux_client::state::get_state(socket_path)) {
        Ok(view) => {
            let sessions = view.snapshot().sessions.len();
            let panes = view.snapshot().resources.len();
            let protocol = format!(
                "client protocol {}.{}.{}",
                phux_protocol::PROTOCOL_VERSION.major,
                phux_protocol::PROTOCOL_VERSION.minor,
                phux_protocol::PROTOCOL_VERSION.patch,
            );
            // A hub that could not reach every satellite is working but incomplete:
            // warn, do not fail.
            if view.is_complete() {
                Check::pass(
                    "server",
                    format!(
                        "reachable at {} ({sessions} session(s), {panes} pane(s)); {protocol}",
                        socket_path.display(),
                    ),
                )
            } else {
                Check::warn(
                    "server",
                    format!(
                        "reachable at {} ({sessions} session(s), {panes} pane(s)); {protocol}; \
                         but this hub could not reach every satellite: {}",
                        socket_path.display(),
                        view.degradation().notices().join("; "),
                    ),
                    "the pane inventory above is incomplete — check the satellite links \
                     with `phux host ls --role satellite`",
                )
            }
        }
        Err(err) => {
            let pid = crate::commands::diagnostics::recovery_pid(&err, socket_path);
            server_probe_failure(socket_path, &err, pid)
        }
    }
}

/// The hint for a socket that exists but did not answer `GET_STATE`.
///
/// A dead socket (`ConnectionRefused` / `NotFound`) may be called stale.
/// A live incompatible server gets the stop-and-start recovery. Anything
/// else gets a neutral hint.
pub(super) fn server_probe_failure(
    socket_path: &std::path::Path,
    err: &phux_client::attach::AttachError,
    pid: Option<i32>,
) -> Check {
    use crate::commands::diagnostics::{
        STALE_SOCKET_HINT, SocketFailure, UNANSWERED_SOCKET_HINT, socket_failure,
    };
    match socket_failure(socket_path, err, pid) {
        SocketFailure::Incompatible(lines) => Check::fail(
            "server",
            lines.first().cloned().unwrap_or_default(),
            lines.into_iter().skip(1).collect::<Vec<_>>().join("\n"),
        ),
        SocketFailure::Dead => Check::fail(
            "server",
            format!(
                "socket {} exists but did not answer: {err}",
                socket_path.display()
            ),
            STALE_SOCKET_HINT,
        ),
        SocketFailure::Other => Check::fail(
            "server",
            format!(
                "socket {} exists but did not answer: {err}",
                socket_path.display()
            ),
            UNANSWERED_SOCKET_HINT,
        ),
    }
}

/// Do the configured plugin manifests load?
fn check_plugins() -> Check {
    match valid_manifest_count() {
        Ok(0) => Check::pass("plugins", "none configured"),
        Ok(n) => Check::pass("plugins", format!("{n} manifest(s) valid")),
        Err(err) => Check::fail(
            "plugins",
            err,
            "run `phux plugin validate` to see which manifest is at fault",
        ),
    }
}

/// Is the installed Claude shim the one this binary writes? The shim is a
/// script written once at install time, so upgrading phux does not upgrade it,
/// and old schemas misbehave (e.g. schema 1 stands the detector down). Warn,
/// never fail: doctor's exit code gates scripts unrelated to Claude.
fn check_agent_shim() -> Check {
    use crate::commands::agent::shim;

    let Some(path) = shim::installed_shim_path() else {
        return Check::warn(
            "agent-shim",
            "cannot tell where the claude-in-phux shim would live because HOME is unset",
            "set HOME and re-run `phux doctor`",
        );
    };
    shim_check(shim::installed_shim_schema(&path), shim::SHIM_SCHEMA, &path)
}

/// The pure half of [`check_agent_shim`].
fn shim_check(installed: Option<u32>, current: u32, path: &std::path::Path) -> Check {
    let where_ = path.display();
    match installed {
        // `install-claude` is opt-in; never installed is normal.
        None => Check::pass("agent-shim", "no claude-in-phux shim installed"),
        Some(found) if found == current => Check::pass(
            "agent-shim",
            format!("claude-in-phux shim at {where_} is current (schema {current})"),
        ),
        Some(found) if found < current => Check::warn(
            "agent-shim",
            format!(
                "claude-in-phux shim at {where_} is schema {found}, but this phux writes \
                 schema {current}"
            ),
            format!(
                "re-run `phux agent install-claude` — {}",
                stale_shim_consequence(found)
            ),
        ),
        // Newer on disk than this binary writes: an older phux.
        Some(found) => Check::warn(
            "agent-shim",
            format!(
                "claude-in-phux shim at {where_} is schema {found}, newer than the schema \
                 {current} this phux writes"
            ),
            "this binary is older than the installed shim: upgrade it with `phux update`, \
             or re-run `phux agent install-claude` to pin the shim to this binary",
        ),
    }
}

/// What the user is living with, per stale schema (tracks the
/// `install-claude` upgrade notice).
const fn stale_shim_consequence(found: u32) -> &'static str {
    match found {
        0 | 1 => {
            "schema 1 declares an agent state on every Claude hook, which stands the \
             server-side detector down for the whole session, so a dead Claude keeps a \
             live badge"
        }
        2 => {
            "schema 2 rewrites the agent record on every Claude hook, which resets the \
             detected state at the end of every turn and makes `phux agent wait` report \
             the agent as departed"
        }
        3 => {
            "schema 3 leaves lifecycle timing to screen detection and cannot publish the \
             Claude Stop hook's exact `done` edge"
        }
        4 => {
            "schema 4 never reads the hook payload, so it cannot open or feed the pane's \
             agent session stream and `PreToolUse`/`PostToolUse` are not wired"
        }
        5 => {
            "schema 5 feeds lifecycle records only, so the pane's agent session stream \
             carries no transcript of prompts, tool results, or replies"
        }
        _ => "the installed shim predates this binary's wrapper behavior",
    }
}

/// Where would a crash have been logged, and could it have been? Paths come
/// from `phux_server::telemetry`, shared with `phux logs`. Only an unwritable
/// state dir warns; the probe is read-only.
fn check_logs() -> Check {
    check_logs_at(
        &phux_server::telemetry::state_dir(),
        &phux_server::telemetry::server_log_path(),
    )
}

/// [`check_logs`] against explicit paths.
fn check_logs_at(state_dir: &std::path::Path, server_log: &std::path::Path) -> Check {
    let clients = crate::commands::logs::client_log_paths(state_dir).map_or(0, |paths| paths.len());
    let server = if server_log.exists() {
        format!("server log {}", server_log.display())
    } else {
        format!("server log {} (not created yet)", server_log.display())
    };
    let detail = format!(
        "{server}; {clients} client log(s); state dir {}",
        state_dir.display()
    );

    // `readonly()` is a plain stat (no write bits); a missing dir is normal.
    let unwritable =
        std::fs::metadata(state_dir).is_ok_and(|metadata| metadata.permissions().readonly());
    if unwritable {
        Check::warn(
            "logs",
            format!("{detail} — state dir is not writable"),
            format!(
                "the next crash would leave no log; restore write access with \
                 `chmod u+w {}`",
                state_dir.display()
            ),
        )
    } else {
        Check::pass("logs", detail)
    }
}

// ---------------------------------------------------------------------------
// output
// ---------------------------------------------------------------------------

fn report_human(checks: &[Check]) -> ExitCode {
    use std::io::IsTerminal as _;
    let color = crate::output::color_enabled(std::io::stdout().is_terminal());
    out!("{}", render_human(checks, color));

    let failed = checks.iter().filter(|c| c.status == Status::Fail).count();
    let warned = checks.iter().filter(|c| c.status == Status::Warn).count();
    outln!();
    if failed > 0 {
        outln!(
            "{}",
            crate::output::human_line(
                crate::output::Severity::Error,
                format_args!("{failed} failed, {warned} warning(s)"),
                color
            )
        );
        return ExitCode::FAILURE;
    }
    if warned > 0 {
        outln!(
            "{}",
            crate::output::human_line(
                crate::output::Severity::Warn,
                format_args!("no failures, {warned} warning(s)"),
                color
            )
        );
    } else {
        outln!(
            "{}",
            crate::output::human_line(
                crate::output::Severity::Ok,
                format_args!("all checks passed"),
                color
            )
        );
    }
    ExitCode::SUCCESS
}

/// The check rows as the human report prints them. The name column is as
/// wide as the longest check name, so a long name (`workload-authority`)
/// cannot push its detail out of alignment with every other row.
fn render_human(checks: &[Check], color: bool) -> String {
    use std::fmt::Write as _;

    let width = checks.iter().map(|c| c.name.len()).max().unwrap_or(0);
    let mut out = String::new();
    for check in checks {
        let severity = check.status.severity();
        let padding = 5 - severity.label().len();
        let line = crate::output::human_line(
            severity,
            format_args!("{:padding$}{:<width$} {}", "", check.name, check.detail),
            color,
        );
        let _ = writeln!(out, "{line}");
        if let Some(hint) = &check.hint {
            for line in hint.lines() {
                let _ = writeln!(out, "  hint: {}: {}", check.name, line.trim_start());
            }
        }
    }
    out
}

fn report_json(checks: &[Check]) -> ExitCode {
    let rows: Vec<_> = checks
        .iter()
        .map(|check| {
            serde_json::json!({
                "name": check.name,
                "status": check.status.as_str(),
                "detail": check.detail,
                "hint": check.hint,
            })
        })
        .collect();
    let failed = checks.iter().filter(|c| c.status == Status::Fail).count();
    let doc = serde_json::json!({
        "schema_version": 1,
        "ok": failed == 0,
        "failed": failed,
        "checks": rows,
    });
    match serde_json::to_string_pretty(&doc) {
        Ok(rendered) => {
            outln!("{rendered}");
            if failed == 0 {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
        Err(err) => crate::commands::json_err::emit(
            true,
            &crate::commands::json_err::CliError::new(
                crate::commands::json_err::codes::JSON_SERIALIZE,
                format!("could not render doctor JSON: {err}"),
                "this is a phux bug worth filing",
            ),
            1,
        ),
    }
}

#[cfg(test)]
mod tests;
