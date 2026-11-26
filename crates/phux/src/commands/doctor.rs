//! `phux doctor` — one command that answers "why isn't this working?", by
//! composing the checks behind `config check`, `plugin validate`, the socket
//! length guard, a `GET_STATE` probe, and the log inventory.
//!
//! Two rules: a check that cannot run reports [`Status::Warn`], never `Pass`;
//! and nothing here mutates anything.

use std::path::PathBuf;
use std::process::ExitCode;

use phux_config::socket::default_socket_path;

use crate::commands::{cli_runtime, plugin::valid_manifest_count};

mod agents;
mod audit;
mod checklist;
mod credentials;
pub(crate) mod other_host;
mod permissions;
mod remote;
mod remote_cert;
mod render;
mod server_health;
mod supervision;

#[cfg(test)]
use agents::shim_check;
use agents::{check_agent_shim, check_skill};
use audit::check_audit;
use credentials::{check_client_certs, check_token_store, check_workload_authority};
use permissions::check_permissions;
use remote::{check_remote_listeners, check_remote_reachable, check_remote_replies};
use remote_cert::{check_remote_cert, check_remote_cert_chain};
use render::count;
use server_health::check_server_health;
use supervision::check_supervision;

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
    const fn mark(self) -> render::Mark {
        match self {
            Self::Pass => render::Mark::OK,
            Self::Warn => render::Mark::WARN,
            Self::Fail => render::Mark::FAIL,
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
    checks.extend(check_supervision(&socket_path));
    checks.extend(check_permissions(&socket_path));
    checks.extend([
        check_plugins(),
        check_agent_shim(),
        check_skill(),
        check_remote_cert(),
        check_token_store(),
        check_workload_authority(),
        check_client_certs(),
        check_remote_listeners(&socket_path),
        check_remote_reachable(&socket_path),
        check_logs(),
        check_audit(&socket_path),
    ]);
    checks.extend(check_remote_replies(&socket_path));
    checks.extend(check_remote_cert_chain());
    let checklist = checklist::items(&checklist::gather(&socket_path, &checks));

    if json {
        return report_json(&checks, &checklist);
    }
    report_human(&checks, &checklist)
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
        "a development build, not an installed release"
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
            let sessions = count(view.snapshot().sessions.len(), "session", "sessions");
            let panes = count(view.snapshot().resources.len(), "pane", "panes");
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
                        "reachable at {} ({sessions}, {panes}); {protocol}",
                        socket_path.display(),
                    ),
                )
            } else {
                Check::warn(
                    "server",
                    format!(
                        "reachable at {} ({sessions}, {panes}); {protocol}; \
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
        Ok(n) => Check::pass(
            "plugins",
            format!("{} valid", count(n, "manifest", "manifests")),
        ),
        Err(err) => Check::fail(
            "plugins",
            err,
            "run `phux plugin validate` to see which manifest is at fault",
        ),
    }
}

/// Where would a crash have been logged, and could it have been? Paths come
/// from `phux_server::telemetry`, shared with `phux logs`. Only an unwritable
/// state dir warns; the probe is read-only.
fn check_logs() -> Check {
    check_logs_at(
        &phux_config::instance::state_dir(),
        &phux_config::instance::server_log_path(),
    )
}

/// [`check_logs`] against explicit paths.
fn check_logs_at(state_dir: &std::path::Path, server_log: &std::path::Path) -> Check {
    let clients = count(
        crate::commands::logs::client_log_paths(state_dir).map_or(0, |paths| paths.len()),
        "client log",
        "client logs",
    );
    let server = if server_log.exists() {
        format!("server log {}", server_log.display())
    } else {
        format!("server log {} (not created yet)", server_log.display())
    };
    let detail = format!("{server}; {clients}; state dir {}", state_dir.display());

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

fn report_human(checks: &[Check], checklist: &[checklist::Item]) -> ExitCode {
    use std::io::IsTerminal as _;
    let color = crate::output::color_enabled(std::io::stdout().is_terminal());
    out!("{}", render_report(checks, checklist, color));
    exit_code(checks)
}

/// The whole human report: the check rows, the checklist, then the summary.
fn render_report(checks: &[Check], checklist: &[checklist::Item], color: bool) -> String {
    format!(
        "{}{}\n{}\n",
        render_human(checks, color),
        checklist::render_human(checklist, color),
        summary(checks),
    )
}

/// Exit 1 when any check failed; warnings alone exit 0.
fn exit_code(checks: &[Check]) -> ExitCode {
    if checks.iter().any(|c| c.status == Status::Fail) {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

/// The report's last line: failures first, then warnings.
fn summary(checks: &[Check]) -> String {
    let tally = |status| checks.iter().filter(|c| c.status == status).count();
    let (failed, warned) = (tally(Status::Fail), tally(Status::Warn));
    let failures = count(failed, "failure", "failures");
    let warnings = count(warned, "warning", "warnings");
    match (failed, warned) {
        (0, 0) => "all checks passed".to_owned(),
        (0, _) => format!("{warnings}, no failures"),
        (_, 0) => failures,
        _ => format!("{failures}, {warnings}"),
    }
}

/// The check rows as the human report prints them. The name column is as
/// wide as the longest check name, so a long name (`workload-authority`)
/// cannot push its detail out of alignment with every other row.
fn render_human(checks: &[Check], color: bool) -> String {
    let rows: Vec<_> = checks
        .iter()
        .map(|check| render::Row {
            mark: check.status.mark(),
            name: check.name,
            detail: &check.detail,
            label: "hint",
            notes: check
                .hint
                .iter()
                .flat_map(|hint| hint.lines())
                .map(str::to_owned)
                .collect(),
        })
        .collect();
    render::rows(&rows, color)
}

fn report_json(checks: &[Check], checklist: &[checklist::Item]) -> ExitCode {
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
        "checklist": checklist::to_json(checklist),
    });
    let code = crate::output::json(&doc);
    if code == ExitCode::SUCCESS && failed > 0 {
        return ExitCode::FAILURE;
    }
    code
}

/// Run `phux doctor` as parsed.
#[allow(
    clippy::needless_pass_by_value,
    reason = "every verb entry point takes its parsed Args by value, as dispatch hands them over"
)]
pub(crate) fn run(
    args: crate::commands::DoctorArgs,
    socket: Option<std::path::PathBuf>,
) -> ExitCode {
    let crate::commands::DoctorArgs { json, remote } = args;
    match remote.remote {
        Some(_) => other_host::run(json, remote.with_socket(socket)),
        None => run_doctor(json, socket),
    }
}

#[cfg(test)]
mod tests;
