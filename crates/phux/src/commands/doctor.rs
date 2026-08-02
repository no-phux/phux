//! `phux doctor` — one command that answers "why isn't this working?".
//!
//! Every check here already existed as its own verb: `config check`,
//! `plugin validate`, a socket-length guard buried in the spawn path, a
//! `GET_STATE` probe inside `ls`, the log-path inventory behind
//! `phux logs`. Knowing to run all of them, in the right order, and how
//! to read each one, is exactly the knowledge a person debugging phux
//! does not have — that is the whole problem. So this composes them and
//! reports one verdict.
//!
//! Two rules keep it honest:
//!
//! * **A check that cannot run is not a check that passed.** Every check
//!   reports [`Status::Warn`] rather than `Pass` when its precondition is
//!   missing, so "no server running" never renders as a green tick.
//! * **Nothing here mutates anything.** A diagnostic that repairs things is
//!   a diagnostic nobody can trust to describe the system.

use std::path::PathBuf;
use std::process::ExitCode;

use phux_server::runtime::default_socket_path;

use crate::commands::{cli_runtime, plugin::valid_manifest_count};

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
    /// Fixed-width marker so the report scans as a column.
    const fn marker(self) -> &'static str {
        match self {
            Self::Pass => "ok  ",
            Self::Warn => "warn",
            Self::Fail => "FAIL",
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

/// `phux doctor [--json] [--socket PATH]`.
///
/// Exit codes: 0 when nothing failed (warnings do not fail the run — a
/// stopped server is a normal state, not a broken install), 1 when any check
/// failed.
pub(crate) fn run_doctor(json: bool, socket: Option<PathBuf>) -> ExitCode {
    let socket_path = socket.unwrap_or_else(default_socket_path);
    let checks = vec![
        check_config(),
        check_socket_path(&socket_path),
        check_server(&socket_path),
        check_plugins(),
        check_logs(),
    ];

    if json {
        return report_json(&checks);
    }
    report_human(&checks)
}

// ---------------------------------------------------------------------------
// checks
// ---------------------------------------------------------------------------

/// Does the config parse, and does every key exist in the schema?
///
/// Reuses `phux config check`, so the two can never disagree about what a
/// valid config is.
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

/// Will the socket path fit in a `sockaddr_un`?
///
/// This one earns its place: the failure mode is a connect that times out
/// with no explanation, and the cause is a path length limit nobody thinks
/// about until they hit it (phux-iwuc).
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

/// Is a server running, and does it speak a protocol this binary knows?
///
/// A stopped server is a `warn`, not a `fail`: running `doctor` before
/// starting phux is a perfectly ordinary thing to do, and a red line there
/// would train people to ignore red lines.
fn check_server(socket_path: &std::path::Path) -> Check {
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
            let panes = view.snapshot().panes.len();
            let protocol = format!(
                "client protocol {}.{}.{}",
                phux_protocol::PROTOCOL_VERSION.major,
                phux_protocol::PROTOCOL_VERSION.minor,
                phux_protocol::PROTOCOL_VERSION.patch,
            );
            // A hub that answered but could not reach a satellite is exactly
            // what `doctor` exists to surface: the server is up, so this is
            // not a FAIL (which would set the exit code and read as "phux is
            // broken"), but reporting PASS would hide the one fact an
            // operator running `phux doctor` on a federated setup is looking
            // for. WARN is the shape that says "working, and here is what is
            // not".
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
                     with `phux satellite ls`",
                )
            }
        }
        // A socket file with nothing behind it is the classic stale-socket
        // case, and it is a real failure: every CLI verb will hang or refuse
        // until it is cleared.
        Err(err) => Check::fail(
            "server",
            format!(
                "socket {} exists but did not answer: {err}",
                socket_path.display()
            ),
            "the socket may be stale — remove it and start a fresh server",
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

/// Where would a crash have been logged, and could it have been?
///
/// Resolves every path through `phux_server::telemetry` — the same helpers
/// the writers use — so this line can never disagree with `phux logs`. An
/// absent log is a normal state (nothing has run yet), so this check never
/// fails; the one thing worth a warning is a state dir that exists but
/// cannot be written, because then the next crash leaves no evidence and
/// nothing else would ever say so. The probe is read-only: creating the dir
/// or a test file to find out would break doctor's nothing-mutates contract.
fn check_logs() -> Check {
    check_logs_at(
        &phux_server::telemetry::state_dir(),
        &phux_server::telemetry::server_log_path(),
    )
}

/// [`check_logs`] against explicit paths, so tests can drive it against a
/// temp dir instead of the real environment.
fn check_logs_at(state_dir: &std::path::Path, server_log: &std::path::Path) -> Check {
    let clients = crate::commands::logs::client_log_paths(state_dir)
        .map(|paths| paths.len())
        .unwrap_or(0);
    let server = if server_log.exists() {
        format!("server log {}", server_log.display())
    } else {
        format!("server log {} (not created yet)", server_log.display())
    };
    let detail = format!(
        "{server}; {clients} client log(s); state dir {}",
        state_dir.display()
    );

    // `readonly()` is a read-only stat: true when no write bit is set at
    // all. The state dir lives under $HOME and is owned by the user, so
    // this catches the realistic case (a stray chmod) without an euid-aware
    // access(2) probe. A dir that does not exist yet is normal — the first
    // writer creates it.
    let unwritable = std::fs::metadata(state_dir)
        .map(|meta| meta.permissions().readonly())
        .unwrap_or(false);
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
    for check in checks {
        outln!(
            "{} {:<12} {}",
            check.status.marker(),
            check.name,
            check.detail
        );
        if let Some(hint) = &check.hint {
            outln!("     {:<12} -> {hint}", "");
        }
    }

    let failed = checks.iter().filter(|c| c.status == Status::Fail).count();
    let warned = checks.iter().filter(|c| c.status == Status::Warn).count();
    outln!();
    if failed > 0 {
        outln!("{failed} failed, {warned} warning(s)");
        return ExitCode::FAILURE;
    }
    if warned > 0 {
        outln!("no failures, {warned} warning(s)");
    } else {
        outln!("all checks passed");
    }
    ExitCode::SUCCESS
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
        Err(err) => {
            eprintln!("phux: could not render doctor JSON: {err}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An over-long socket path is the failure this check exists for: the
    /// symptom is an unexplained connect timeout, and nobody guesses
    /// `sockaddr_un` on their own.
    #[test]
    fn an_over_long_socket_path_fails_with_a_hint() {
        let long = PathBuf::from(format!("/tmp/{}/phux.sock", "x".repeat(200)));
        let check = check_socket_path(&long);
        assert_eq!(check.status, Status::Fail);
        assert!(
            check.hint.is_some(),
            "a failure with no hint is not a diagnosis"
        );
    }

    /// A workable path passes and echoes the path, so the report says which
    /// socket it actually checked.
    #[test]
    fn a_short_socket_path_passes_and_names_itself() {
        let check = check_socket_path(std::path::Path::new("/tmp/phux-doctor-test.sock"));
        assert_eq!(check.status, Status::Pass);
        assert!(check.detail.contains("phux-doctor-test.sock"));
    }

    /// A stopped server must not read as broken. Someone running `doctor`
    /// before starting phux is doing a normal thing, and a red line there
    /// teaches people to ignore red lines.
    #[test]
    fn a_missing_server_warns_rather_than_fails() {
        let check = check_server(std::path::Path::new("/tmp/phux-doctor-absent-server.sock"));
        assert_eq!(check.status, Status::Warn);
    }

    /// Warnings alone exit 0; a warning is "could not verify", not "broken".
    #[test]
    fn warnings_alone_do_not_fail_the_run() {
        let checks = vec![
            Check::pass("a", "fine"),
            Check::warn("b", "unknown", "do something"),
        ];
        assert!(checks.iter().all(|c| c.status != Status::Fail));
        assert_eq!(report_human(&checks), ExitCode::SUCCESS);
    }

    /// Any failure fails the run, so `phux doctor` can gate a setup script.
    #[test]
    fn one_failure_fails_the_run() {
        let checks = vec![
            Check::pass("a", "fine"),
            Check::fail("b", "broken", "fix it"),
        ];
        assert_eq!(report_human(&checks), ExitCode::FAILURE);
    }

    /// Every non-pass carries a hint. A diagnosis that names a problem
    /// without naming a next step is half a diagnosis.
    #[test]
    fn every_non_pass_constructor_carries_a_hint() {
        assert!(Check::warn("n", "d", "h").hint.is_some());
        assert!(Check::fail("n", "d", "h").hint.is_some());
        assert!(Check::pass("n", "d").hint.is_none());
    }

    /// The logs line on a machine where things have run: it names the
    /// server log, counts the client logs, and names the state dir — the
    /// three facts a crash investigation starts from.
    #[test]
    #[allow(clippy::unwrap_used, reason = "test code")]
    fn logs_check_names_paths_and_counts_clients() {
        let dir = tempfile::tempdir().unwrap();
        let server_log = dir.path().join("server.log");
        std::fs::write(&server_log, b"started\n").unwrap();
        std::fs::write(dir.path().join("client-100.log"), b"a\n").unwrap();
        std::fs::write(dir.path().join("client-200.log"), b"b\n").unwrap();

        let check = check_logs_at(dir.path(), &server_log);
        assert_eq!(check.status, Status::Pass);
        assert!(check.detail.contains(&server_log.display().to_string()));
        assert!(check.detail.contains("2 client log(s)"));
        assert!(check.detail.contains(&dir.path().display().to_string()));
        assert!(!check.detail.contains("not created yet"));
    }

    /// A fresh machine — no server has ever run, the state dir may not
    /// even exist — is a normal state, not a problem. The line still names
    /// every path (existence-aware), and the check passes.
    #[test]
    #[allow(clippy::unwrap_used, reason = "test code")]
    fn logs_check_reports_absent_logs_as_normal() {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join("never-created");
        let server_log = state.join("server.log");

        let check = check_logs_at(&state, &server_log);
        assert_eq!(check.status, Status::Pass);
        assert!(check.detail.contains("not created yet"));
        assert!(check.detail.contains("0 client log(s)"));
        assert!(check.detail.contains(&server_log.display().to_string()));
    }

    /// The one thing this check warns about: a state dir that cannot be
    /// written means the next crash leaves no evidence, silently. Warn —
    /// not Fail, phux itself still works — with a hint naming the fix.
    #[cfg(unix)]
    #[test]
    #[allow(clippy::unwrap_used, reason = "test code")]
    fn an_unwritable_state_dir_warns_with_a_hint() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o555)).unwrap();

        let check = check_logs_at(dir.path(), &dir.path().join("server.log"));

        // Restore write access so the tempdir cleanup can do its job.
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755)).unwrap();

        assert_eq!(check.status, Status::Warn);
        assert!(check.detail.contains("not writable"));
        let hint = check
            .hint
            .expect("a warn without a hint is half a diagnosis");
        assert!(hint.contains(&dir.path().display().to_string()));
    }
}
