//! `phux doctor` — one command that answers "why isn't this working?", by
//! composing the checks behind `config check`, `plugin validate`, the socket
//! length guard, a `GET_STATE` probe, and the log inventory.
//!
//! Two rules: a check that cannot run reports [`Status::Warn`], never `Pass`;
//! and nothing here mutates anything.

use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

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

/// Is the server crash-looping, running a stale build, under a legacy
/// (unthrottled) supervisor, or armed but not yet supervised? A restarting
/// server is otherwise indistinguishable from a healthy one (ADR-0080).
/// Every applicable condition is reported; they tend to co-occur.
fn check_server_health(socket_path: &std::path::Path) -> Vec<Check> {
    let unit = legacy_service_unit_path().filter(|path| path.exists());
    let legacy_unit = unit
        .as_deref()
        .filter(|unit| supervisor_unit_is_legacy(unit));

    let crash_loop = phux_server::health::crash_loop()
        .map(|count| (count, phux_server::health::CRASH_LOOP_WINDOW.as_secs() / 60));

    // Armed-but-inactive supervision (ADR-0088), scoped to this socket.
    let armed_unit = super::service::armed_adoption_unit(socket_path);

    // Version skew: the binary was replaced but the server was not restarted.
    let ours = env!("CARGO_PKG_VERSION");
    let theirs = phux_server::health::running_version();
    let version_skew = theirs
        .as_deref()
        .filter(|theirs| *theirs != ours)
        .map(|theirs| (theirs, ours));

    server_health_checks(
        crash_loop,
        legacy_unit,
        armed_unit.as_deref(),
        version_skew,
        || phux_server::health::recent_starts(phux_server::health::CRASH_LOOP_WINDOW).len(),
    )
}

/// The pure half of [`check_server_health`]. `recent_starts` is only read for
/// the pass fallback.
fn server_health_checks(
    crash_loop: Option<(usize, u64)>,
    legacy_unit: Option<&std::path::Path>,
    armed_unit: Option<&std::path::Path>,
    version_skew: Option<(&str, &str)>,
    recent_starts: impl FnOnce() -> usize,
) -> Vec<Check> {
    let mut checks = Vec::new();

    if let Some((count, window_mins)) = crash_loop {
        checks.push(Check::fail(
            "server-health",
            format!(
                "the server started {count} times in the last {window_mins} minutes — it is crash-looping"
            ),
            format!(
                "something is killing the server on startup; the reason is in {}",
                phux_server::telemetry::server_log_path().display()
            ),
        ));
    }

    // A legacy unit restarts on every exit, unthrottled; the remedy is the
    // non-destructive `phux service reconcile`.
    if let Some(unit) = legacy_unit {
        checks.push(Check::warn(
            "server-health",
            format!(
                "the supervisor unit at {} restarts on every exit, unthrottled",
                unit.display()
            ),
            "it resurrects servers you stopped and hides crash-loops — run \
             `phux service reconcile` to correct it in place; nothing is \
             stopped and no pane is lost (on macOS the corrected policy takes \
             effect at your next login, which that command spells out)",
        ));
    }

    // Armed supervision is working as designed, but until the hand-over the
    // running server is not restart-managed; surface it.
    if let Some(unit) = armed_unit {
        checks.push(Check::warn(
            "server-health",
            format!(
                "supervision is armed, not active — the unit at {} is written but \
                 deliberately unloaded while the current server runs",
                unit.display()
            ),
            format!(
                "this is `phux service install --adopt` working as designed: {}",
                super::service::ARMED_SUPERVISION_EXPLANATION
            ),
        ));
    }

    if let Some((theirs, ours)) = version_skew {
        checks.push(Check::warn(
            "server-health",
            format!("the running server is {theirs}; this binary is {ours}"),
            "run `phux upgrade` to hand the server over in place (panes survive), \
             or attach with `phux`, which now does it automatically",
        ));
    }

    if checks.is_empty() {
        checks.push(Check::pass(
            "server-health",
            format!("{} server start(s) in the last hour", recent_starts()),
        ));
    }

    checks
}

/// Whether `unit`'s restart behavior is dangerous: not failure-only, or not
/// throttled to a positive interval. Checks values, not key presence.
///
/// Deliberately looser than [`crate::commands::service::reconcile_unit`]'s
/// byte-exact "current" test, so retuning the throttle constant does not make
/// every previously installed (still safe) unit warn. The tests pin where the
/// two predicates agree and the one case where they may not.
fn supervisor_unit_is_legacy(unit: &std::path::Path) -> bool {
    let Ok(body) = std::fs::read_to_string(unit) else {
        return false;
    };
    !(restart_is_failure_only(&body) && restart_is_throttled(&body))
}

/// Does `body` restart only on abnormal exit (launchd `SuccessfulExit` false,
/// or systemd exactly `Restart=on-failure`)?
fn restart_is_failure_only(body: &str) -> bool {
    if let Some((_, rest)) = body.split_once("<key>SuccessfulExit</key>") {
        return rest.trim_start().starts_with("<false/>");
    }
    if let Some((_, rest)) = body.split_once("Restart=") {
        return value_token(rest) == "on-failure";
    }
    false
}

/// Does `body` throttle restarts to a positive interval (`ThrottleInterval`
/// or `RestartSec` greater than zero)?
fn restart_is_throttled(body: &str) -> bool {
    if let Some((_, rest)) = body.split_once("<key>ThrottleInterval</key>") {
        return plist_integer(rest).is_some_and(|n| n > 0);
    }
    if let Some((_, rest)) = body.split_once("RestartSec=") {
        return leading_digits(value_token(rest)).is_some_and(|n| n > 0);
    }
    false
}

/// The `<integer>N</integer>` following a plist key (`rest` starts after
/// `</key>`).
fn plist_integer(rest_after_key: &str) -> Option<u64> {
    let (_, rest) = rest_after_key.split_once("<integer>")?;
    let (digits, _) = rest.split_once("</integer>")?;
    digits.trim().parse().ok()
}

/// The next whitespace-delimited token after a systemd `Key=`.
fn value_token(rest: &str) -> &str {
    let end = rest.find(|c: char| c.is_whitespace()).unwrap_or(rest.len());
    rest[..end].trim()
}

/// The leading ASCII digits of `value` (reads `30s` or `30`).
fn leading_digits(value: &str) -> Option<u64> {
    let digits: String = value.chars().take_while(char::is_ascii_digit).collect();
    if digits.is_empty() {
        None
    } else {
        digits.parse().ok()
    }
}

/// Where `service install` writes this profile's unit, if this platform has
/// a generator and `HOME` is set.
fn legacy_service_unit_path() -> Option<PathBuf> {
    let manager = super::service::Manager::host()?;
    manager
        .unit_path(super::service::profile_suffix().as_deref())
        .ok()
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
        // A socket file with nothing behind it is a stale socket: a real failure.
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

/// Can the credential store the remote listeners authenticate against be
/// read? A broken store admits no device while everything local stays green.
fn check_token_store() -> Check {
    let path = std::env::var_os("PHUX_WS_TOKENS")
        .map_or_else(phux_server::auth::default_token_store_path, PathBuf::from);
    token_store_check(
        &path,
        phux_server::auth::ReloadingTokenStore::load(path.clone()).err(),
    )
}

/// The pure half of [`check_token_store`].
fn token_store_check(path: &std::path::Path, error: Option<phux_server::auth::AuthError>) -> Check {
    let Some(error) = error else {
        return Check::pass(
            "token-store",
            format!("credential store at {} loads", path.display()),
        );
    };
    let remedy = if error.to_string().contains("legacy") {
        "the server refuses to guess at a pre-versioned store: convert it with \
         `phux pair --migrate-legacy`; the running server re-reads it on the next \
         connection, or restart / `phux upgrade` it if it disabled its listeners at boot"
            .to_owned()
    } else {
        format!(
            "no device can authenticate until this loads; fix the file at {} (or point \
             PHUX_WS_TOKENS elsewhere), then restart or `phux upgrade` a server that \
             disabled its listeners at boot",
            path.display()
        )
    };
    Check::fail(
        "token-store",
        format!(
            "credential store at {} cannot be loaded ({error}) — no device credential \
             can be admitted, and a server that disabled its remote listeners at boot \
             stays down until this loads",
            path.display()
        ),
        remedy,
    )
}

fn check_workload_authority() -> Check {
    let paths = phux_server::workload::WorkloadPaths::from_env();
    workload_authority_check(&paths.ca_cert, &paths.registry)
}

/// The pure half of [`check_workload_authority`]: the CA fingerprint and the
/// registry generation (ADR-0116). It never reads or names the CA key.
fn workload_authority_check(ca_cert: &std::path::Path, registry: &std::path::Path) -> Check {
    use phux_server::workload::{WorkloadError, WorkloadRegistry, ca_fingerprint};
    let fingerprint = match ca_fingerprint(ca_cert) {
        Ok(fingerprint) => fingerprint,
        Err(WorkloadError::AuthorityMissing) => {
            return Check::pass(
                "workload-authority",
                "no workload CA; mTLS workload authority is not initialized",
            );
        }
        Err(error) => {
            return Check::fail(
                "workload-authority",
                format!(
                    "workload CA at {} cannot be read ({error})",
                    ca_cert.display()
                ),
                "a server with PHUX_WORKLOAD_MTLS set disables its remote listeners until the CA loads; fix the file's owner and mode, or remove the pair deliberately and re-enroll every client",
            );
        }
    };
    match WorkloadRegistry::load(registry) {
        Ok(snapshot) => {
            let now = chrono::Utc::now().timestamp();
            let active = snapshot
                .credentials()
                .iter()
                .filter(|credential| credential.is_active_at(now))
                .count();
            Check::pass(
                "workload-authority",
                format!(
                    "workload CA {fingerprint}; registry generation {}, {active} of {} credential(s) active",
                    snapshot.generation(),
                    snapshot.len()
                ),
            )
        }
        Err(error) => Check::fail(
            "workload-authority",
            format!(
                "workload CA {fingerprint}; registry at {} cannot be loaded ({error})",
                registry.display()
            ),
            "a running server admits no workload credential until the registry loads; restore it (owner-only, mode 0600) or re-enroll with `phux workload add-key`",
        ),
    }
}

/// Are the workload client certificates `phux host add` enrolled for this
/// machine's remotes good for a while yet (ADR-0116)? Reads only the public
/// certificates the `[[remote]]` entries name, never a key.
fn check_client_certs() -> Check {
    match crate::commands::remote::load_registry() {
        Ok(entries) => client_certs_check(&entries, chrono::Utc::now().timestamp()),
        Err(err) => Check::warn(
            "client-certs",
            format!("could not read the remote registry ({err})"),
            "fix the config (`phux config check`), then rerun `phux doctor`",
        ),
    }
}

/// The pure half of [`check_client_certs`]: a remote naming half an
/// identity, or a certificate that is unreadable, expired, or within the
/// renewal window, warns with the command that fixes it.
fn client_certs_check(entries: &[crate::commands::remote::RemoteEntry], now: i64) -> Check {
    use crate::commands::enroll::{admission_ends, calendar_date, renewal_due};
    let mut due = Vec::new();
    let mut soonest: Option<i64> = None;
    let mut enrolled = 0_usize;
    for entry in entries {
        let name = &entry.name;
        match (entry.client_cert.as_deref(), entry.client_key.as_deref()) {
            (None, None) => {}
            (Some(cert), Some(_)) => {
                enrolled += 1;
                if let Some(detail) = renewal_due(cert, now) {
                    due.push((
                        format!("{name}: {detail}"),
                        format!("phux host renew {name}"),
                    ));
                } else if let Some(ends) = admission_ends(cert) {
                    soonest = Some(soonest.map_or(ends, |soonest| soonest.min(ends)));
                }
            }
            _ => due.push((
                format!("{name}: names only one of client-cert and client-key"),
                format!("phux host add {}", entry.ssh_destination()),
            )),
        }
    }
    if !due.is_empty() {
        let (details, remedies): (Vec<_>, Vec<_>) = due.into_iter().unzip();
        return Check::warn(
            "client-certs",
            format!("workload client certificate: {}", details.join("; ")),
            format!(
                "run `{}`; a paired server refuses an expired certificate",
                remedies.join("`, `")
            ),
        );
    }
    match soonest {
        None if enrolled == 0 => Check::pass(
            "client-certs",
            "no remote presents an enrolled workload client certificate",
        ),
        None => Check::pass(
            "client-certs",
            format!("{enrolled} remote(s) present a workload client certificate"),
        ),
        Some(ends) => Check::pass(
            "client-certs",
            format!(
                "{enrolled} remote(s) present a workload client certificate; the first expires {}",
                calendar_date(ends)
            ),
        ),
    }
}

/// How long the reachability probe waits for the listener to answer.
const REMOTE_PROBE_TIMEOUT: Duration = Duration::from_secs(4);

/// What dialing our own routable listener revealed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Reachability {
    /// The listener answered (a handshake or an auth refusal both prove packets
    /// reached phux).
    Answered,
    /// The connection was refused, so nothing is bound there.
    NoListener,
    /// Accepted, then silence: the shape of a host packet filter.
    Silent,
    /// The address could not be reached at all.
    Unreachable,
}

/// Did every remote listener the running server expected actually bind? Asks
/// the server over UDS, so a fixed-on-disk store cannot hide a dead remote
/// surface.
fn check_remote_listeners(socket_path: &std::path::Path) -> Check {
    if !socket_path.exists() {
        return Check::warn(
            "remote-listeners",
            "no server to ask about remote listeners",
            "start one with `phux` (auto-spawns) or `phux server`",
        );
    }
    let Ok(rt) = cli_runtime() else {
        return Check::warn(
            "remote-listeners",
            "could not build a runtime to ask the server about remote listeners",
            "retry; if this persists it is a bug worth filing",
        );
    };
    let report = match rt.block_on(phux_client::state::get_state(socket_path)) {
        Ok(view) => view.snapshot().listeners().cloned(),
        Err(err) => {
            return Check::warn(
                "remote-listeners",
                format!("could not ask the server about remote listeners: {err}"),
                "the socket may be stale — remove it and start a fresh server",
            );
        }
    };
    let token_store_ok = phux_server::auth::ReloadingTokenStore::load(
        std::env::var_os("PHUX_WS_TOKENS")
            .map_or_else(phux_server::auth::default_token_store_path, PathBuf::from),
    )
    .is_ok();
    remote_listeners_check(report.as_ref(), token_store_ok)
}

/// The pure half of [`check_remote_listeners`].
fn remote_listeners_check(
    report: Option<&phux_protocol::wire::RemoteListenersReport>,
    token_store_ok: bool,
) -> Check {
    let Some(report) = report else {
        return Check::pass(
            "remote-listeners",
            "server did not publish a remote-listener report (no remote transport configured, \
             or an older server)",
        );
    };
    if report.listeners.is_empty() {
        return Check::pass(
            "remote-listeners",
            "no remote listeners were configured or auto-bound",
        );
    }
    let unhealthy: Vec<_> = report.unhealthy().collect();
    if unhealthy.is_empty() {
        let bound = report
            .listeners
            .iter()
            .filter(|slot| slot.bound)
            .map(|slot| slot.transport.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        return Check::pass(
            "remote-listeners",
            format!("remote listeners bound ({bound})"),
        );
    }
    let detail = unhealthy
        .iter()
        .map(|slot| {
            let reason = slot.disabled_reason.map_or(
                "unknown",
                phux_protocol::wire::ListenerDisabledReason::as_str,
            );
            let addr = slot.addr.as_deref().unwrap_or("?");
            format!("{} at {addr} disabled ({reason})", slot.transport)
        })
        .collect::<Vec<_>>()
        .join("; ");
    let needs_restart = token_store_ok
        && unhealthy.iter().any(|slot| {
            slot.disabled_reason
                == Some(phux_protocol::wire::ListenerDisabledReason::TokenStoreLoadFailed)
        });
    let hint = if needs_restart {
        "the credential store loads now, but this server disabled the listeners at boot — \
         restart or `phux upgrade` so they re-bind"
            .to_owned()
    } else if unhealthy.iter().any(|slot| {
        slot.disabled_reason
            == Some(phux_protocol::wire::ListenerDisabledReason::TokenStoreLoadFailed)
    }) {
        "fix the credential store (`phux pair --migrate-legacy` for a pre-versioned file), \
         then restart or `phux upgrade` the server"
            .to_owned()
    } else {
        "check the server log for the bind error, then restart or `phux upgrade` after fixing it"
            .to_owned()
    };
    Check::fail(
        "remote-listeners",
        format!("remote surface down: {detail}"),
        hint,
    )
}

/// Does traffic to this server's routable wss listener actually reach it?
/// Dials the bound off-loopback address with the real client stack, catching
/// a firewall stealth-drop that every local check reports as healthy. A
/// loopback-only bind is skipped; `0.0.0.0`/`::` is rewritten onto a detected
/// overlay IP, otherwise warned.
fn check_remote_reachable(socket_path: &std::path::Path) -> Check {
    match server_wss_offer(socket_path) {
        WssOffer::Disabled(reason) => remote_reachable_check(
            "this server's wss listener",
            Reachability::NoListener,
            Some(reason),
        ),
        WssOffer::Absent => Check::pass(
            "remote-reachable",
            "this server has no wss listener; nothing routable to probe",
        ),
        WssOffer::Bound { addr } => {
            let overlay = if bound_needs_overlay(addr.as_deref()) {
                phux_config::overlay::detect()
            } else {
                Vec::new()
            };
            remote_reachable_verdict(off_loopback_target(addr.as_deref(), &overlay), |url| {
                probe_remote_listener(url)
            })
        }
    }
}

/// What this instance's `GET_STATE` says about its wss listener.
#[derive(Debug, Clone)]
enum WssOffer {
    /// A bound, healthy wss slot, with the address the server recorded.
    Bound { addr: Option<String> },
    /// A wss slot exists and the server disabled it.
    Disabled(phux_protocol::wire::ListenerDisabledReason),
    /// No server, no listener table, or no wss slot.
    Absent,
}

/// Where the off-loopback reachability probe should dial.
#[derive(Debug, Clone, PartialEq, Eq)]
enum OffLoopbackTarget {
    /// Concrete non-loopback address belonging to *this* server.
    Dial(SocketAddr),
    /// Bound on loopback only — the Application Firewall does not apply.
    LoopbackOnly,
    /// Unspecified or unknown bind, and no overlay IP to rewrite onto.
    NoOffLoopback { bound: String },
}

fn bound_needs_overlay(bound: Option<&str>) -> bool {
    bound
        .and_then(|raw| raw.parse::<SocketAddr>().ok())
        .is_some_and(|addr| addr.ip().is_unspecified())
}

/// Pick a dial target from this server's bound wss address.
fn off_loopback_target(bound: Option<&str>, overlay: &[IpAddr]) -> OffLoopbackTarget {
    let Some(raw) = bound else {
        return OffLoopbackTarget::NoOffLoopback {
            bound: "unknown".into(),
        };
    };
    let Ok(addr) = raw.parse::<SocketAddr>() else {
        return OffLoopbackTarget::NoOffLoopback {
            bound: raw.to_owned(),
        };
    };
    if addr.ip().is_loopback() {
        return OffLoopbackTarget::LoopbackOnly;
    }
    if addr.ip().is_unspecified() {
        return overlay
            .iter()
            .copied()
            .find(|ip| !ip.is_loopback() && !ip.is_unspecified() && ip.is_ipv4() == addr.is_ipv4())
            .map_or_else(
                || OffLoopbackTarget::NoOffLoopback {
                    bound: raw.to_owned(),
                },
                |ip| OffLoopbackTarget::Dial(SocketAddr::new(ip, addr.port())),
            );
    }
    OffLoopbackTarget::Dial(addr)
}

/// The pure half of the bound-address verdict.
fn remote_reachable_verdict(
    target: OffLoopbackTarget,
    probe: impl FnOnce(&str) -> Reachability,
) -> Check {
    match target {
        OffLoopbackTarget::LoopbackOnly => Check::pass(
            "remote-reachable",
            "wss is bound on loopback only; inbound host-firewall rules do not apply",
        ),
        OffLoopbackTarget::NoOffLoopback { bound } => Check::warn(
            "remote-reachable",
            format!(
                "wss is bound on {bound}; no off-loopback address to probe, so a \
                 host firewall stealth-drop would look healthy"
            ),
            "doctor only probes a concrete non-loopback bind, or an overlay IP \
             rewritten onto 0.0.0.0/:: — bind `--listen` to a routable address, \
             or check PHUX_OVERLAY_ADDRS (for Tailscale, `tailscale ip -4`)",
        ),
        OffLoopbackTarget::Dial(addr) => {
            let url = format!("wss://{addr}");
            remote_reachable_check(&url, probe(&url), None)
        }
    }
}

/// Ask the running server what it is doing with wss.
fn server_wss_offer(socket_path: &std::path::Path) -> WssOffer {
    if !socket_path.exists() {
        return WssOffer::Absent;
    }
    let Ok(rt) = cli_runtime() else {
        return WssOffer::Absent;
    };
    let Ok(view) = rt.block_on(phux_client::state::get_state(socket_path)) else {
        return WssOffer::Absent;
    };
    let Some(report) = view.snapshot().listeners() else {
        return WssOffer::Absent;
    };
    let Some(slot) = report
        .listeners
        .iter()
        .find(|slot| slot.transport == phux_protocol::wire::RemoteListenerTransport::Wss)
    else {
        return WssOffer::Absent;
    };
    if slot.is_unhealthy() {
        return slot
            .disabled_reason
            .map_or(WssOffer::Absent, WssOffer::Disabled);
    }
    if slot.bound {
        WssOffer::Bound {
            addr: slot.addr.clone(),
        }
    } else {
        WssOffer::Absent
    }
}

/// Dial `url` and classify the answer. No token and no cert verification:
/// this asks only whether packets land.
fn probe_remote_listener(url: &str) -> Reachability {
    let Ok(runtime) = cli_runtime() else {
        return Reachability::Unreachable;
    };
    let dial = phux_dial::ws::WsDial {
        url: url.to_owned(),
        token: None,
        trust: phux_dial::CertTrust::SkipVerify,
        tls_server_name: None,
        identity: None,
    };
    let probe = async {
        let Ok(outcome) =
            tokio::time::timeout(REMOTE_PROBE_TIMEOUT, phux_dial::ws::dial(&dial)).await
        else {
            return Reachability::Silent;
        };
        match outcome {
            Err(phux_dial::DialError::Unreachable(err)) => {
                if err.to_lowercase().contains("refused") {
                    Reachability::NoListener
                } else {
                    Reachability::Unreachable
                }
            }
            Ok(_) | Err(_) => Reachability::Answered,
        }
    };
    runtime.block_on(probe)
}

/// The pure half of [`check_remote_reachable`].
fn remote_reachable_check(
    url: &str,
    reachability: Reachability,
    server_disabled: Option<phux_protocol::wire::ListenerDisabledReason>,
) -> Check {
    match reachability {
        Reachability::Answered => Check::pass(
            "remote-reachable",
            format!("{url} answered; remote clients can reach this server"),
        ),
        Reachability::Silent => Check::fail(
            "remote-reachable",
            format!(
                "{url} accepted a connection and then answered nothing — UDS is healthy \
                 and the listener is bound, so this is a host firewall stealth-drop, not \
                 a dead server"
            ),
            FIREWALL_REMEDY,
        ),
        Reachability::NoListener => {
            let hint = match server_disabled {
                Some(phux_protocol::wire::ListenerDisabledReason::TokenStoreLoadFailed) => {
                    "the running server disabled wss because the credential store failed to \
                     load at boot — fix the store, then restart or `phux upgrade`"
                        .to_owned()
                }
                Some(reason) => {
                    format!(
                        "the running server reports wss disabled ({}) — check the server log, \
                         then restart or `phux upgrade` after fixing it",
                        reason.as_str()
                    )
                }
                None => "expected remote access? run `phux pair` — the listener only auto-binds \
                     once a device credential exists"
                    .to_owned(),
            };
            Check::warn(
                "remote-reachable",
                format!("nothing is listening on {url}"),
                hint,
            )
        }
        Reachability::Unreachable => Check::warn(
            "remote-reachable",
            format!("could not reach {url} from this host"),
            "if this is a VPN address, check its tunnel and routes (Tailscale: `tailscale status`)",
        ),
    }
}

/// What to do about a listener that is bound but unreachable. On macOS the
/// Application Firewall drops packets to the adhoc-signed binary, and its
/// allowlist is keyed to a path that changes on every Homebrew upgrade.
#[cfg(target_os = "macos")]
const FIREWALL_REMEDY: &str = "macOS: Application Firewall stealth-drops inbound packets to \
     unrecognized binaries, and phux is adhoc-signed. Allowlisting is per exact path \
     (Homebrew `/opt/homebrew/Cellar/phux/<version>/bin/phux`), so every upgrade breaks it. \
     Check with `/usr/libexec/ApplicationFirewall/socketfilterfw --getglobalstate`. On a \
     host that already lives behind an overlay, turning the firewall off is the durable \
     workaround until signed/notarized releases";

#[cfg(not(target_os = "macos"))]
const FIREWALL_REMEDY: &str = "check this host's packet filter for a rule dropping inbound \
     connections to phux's listener port";

fn check_remote_cert() -> Check {
    let operator_cert = std::env::var_os("PHUX_WS_TLS_CERT").is_some()
        || std::env::var_os("PHUX_WS_TLS_KEY").is_some();
    let cert = std::env::var_os("PHUX_WS_TLS_CERT").map_or_else(
        phux_server::transport::tls::default_cert_path,
        PathBuf::from,
    );
    let key = std::env::var_os("PHUX_WS_TLS_KEY")
        .map_or_else(phux_server::transport::tls::default_key_path, PathBuf::from);
    // Same overlay detection `phux pair` uses (ADR-0037).
    let advertised: Vec<String> = phux_config::overlay::detect()
        .into_iter()
        .map(phux_server::transport::tls::san_name)
        .collect();
    remote_cert_check(&cert, &key, &advertised, operator_cert)
}

/// Does the remote certificate name the advertised overlay address? SANs are
/// fixed at generation, and regenerating rotates the pinned fingerprint
/// (ADR-0091). Warn, never fail: phux consumers pin the fingerprint.
fn remote_cert_check(
    cert: &std::path::Path,
    key: &std::path::Path,
    advertised: &[String],
    operator_cert: bool,
) -> Check {
    let source = if operator_cert {
        "operator-supplied"
    } else {
        "auto-provisioned"
    };
    if !cert.exists() {
        return Check::warn(
            "remote-cert",
            format!("no {source} certificate at {}", cert.display()),
            "run `phux pair` — it provisions the certificate and mints a device token",
        );
    }
    if advertised.is_empty() {
        return Check::warn(
            "remote-cert",
            format!(
                "{source} certificate at {}; no overlay address detected, so its \
                 coverage of a routable address cannot be checked",
                cert.display()
            ),
            "nothing to do unless you expected an overlay: check PHUX_OVERLAY_ADDRS or `tailscale ip -4`",
        );
    }
    match phux_server::transport::tls::uncovered_names(cert, advertised) {
        Err(err) => Check::fail(
            "remote-cert",
            format!("cannot read {}: {err}", cert.display()),
            "the remote listener will not start; remove the unreadable file and \
             re-run `phux pair`",
        ),
        Ok(uncovered) if uncovered.is_empty() => Check::pass(
            "remote-cert",
            format!(
                "{source} certificate at {} names {}",
                cert.display(),
                advertised.join(", ")
            ),
        ),
        Ok(uncovered) => Check::warn(
            "remote-cert",
            format!(
                "{source} certificate at {} does not name {} — fingerprint-pinning \
                 devices are unaffected, but a client that validates the server name \
                 (a browser, or curl --cacert) will refuse the handshake",
                cert.display(),
                uncovered.join(", ")
            ),
            if operator_cert {
                "reissue the certificate with those addresses in its subjectAltName".to_owned()
            } else {
                format!(
                    "regenerating is the only fix and it rotates the pinned fingerprint, \
                     un-pairing every paired device: `rm {} {} && phux pair`, then re-pair \
                     each device",
                    cert.display(),
                    key.display()
                )
            },
        ),
    }
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
    out!("{}", render_human(checks));

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

/// The check rows as the human report prints them. The name column is as
/// wide as the longest check name, so a long name (`workload-authority`)
/// cannot push its detail out of alignment with every other row.
fn render_human(checks: &[Check]) -> String {
    use std::fmt::Write as _;

    let width = checks.iter().map(|c| c.name.len()).max().unwrap_or(0);
    let mut out = String::new();
    for check in checks {
        let _ = writeln!(
            out,
            "{} {:<width$} {}",
            check.status.marker(),
            check.name,
            check.detail
        );
        if let Some(hint) = &check.hint {
            let _ = writeln!(out, "     {:<width$} -> {hint}", "");
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
mod tests {
    use super::*;

    /// An unloadable (pre-versioned) store fails and names the migration.
    #[test]
    fn an_unloadable_credential_store_fails_and_names_the_fix() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = dir.path().join("remote-tokens");

        assert_eq!(token_store_check(&store, None).status, Status::Pass);

        std::fs::write(&store, "deadbeef\n").expect("write legacy line");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&store, std::fs::Permissions::from_mode(0o600))
                .expect("the store rejects group/world-readable modes first");
        }
        let legacy = phux_server::auth::ReloadingTokenStore::load(store.clone())
            .expect_err("a pre-versioned store must refuse to load");

        let check = token_store_check(&store, Some(legacy));
        assert_eq!(
            check.status,
            Status::Fail,
            "a store that admits no device is a failure, not a warning"
        );
        assert!(
            check.detail.contains("remote listener"),
            "the detail must say what was lost, not just that a file is bad: {}",
            check.detail
        );
        let hint = check.hint.expect("a failure must carry a remedy");
        assert!(
            hint.contains("phux pair --migrate-legacy"),
            "the remedy for a pre-versioned store must name the migration command: {hint}"
        );
        assert!(
            hint.contains("restart") || hint.contains("upgrade"),
            "a server that predates lenient loading needs a restart to re-bind: {hint}"
        );
    }

    /// A bound-but-silent listener fails with a firewall remedy; benign states
    /// warn; a server-reported disable points at restart, not pairing.
    #[test]
    fn a_bound_but_silent_listener_fails_with_an_actionable_remedy() {
        let url = "wss://100.64.0.2:8787";

        let check = remote_reachable_check(url, Reachability::Silent, None);
        assert_eq!(
            check.status,
            Status::Fail,
            "a listener that answers nothing is a failure, not a warning: \
             remote access is entirely broken"
        );
        assert!(
            check.detail.contains("UDS"),
            "the detail must contrast UDS-healthy with the silent remote: {}",
            check.detail
        );
        assert!(
            check.detail.contains("firewall") || check.detail.contains("stealth"),
            "the detail must name the failure mode, not a dead server: {}",
            check.detail
        );
        let hint = check.hint.expect("a failure must carry a remedy");
        assert!(
            hint.contains("firewall") || hint.contains("packet filter"),
            "the remedy must name the blocker: {hint}"
        );
        #[cfg(target_os = "macos")]
        {
            assert!(
                hint.contains("adhoc") && hint.contains("Cellar"),
                "macOS remedy must name the adhoc signature and the versioned Cellar path: {hint}"
            );
        }

        assert_eq!(
            remote_reachable_check(url, Reachability::Answered, None).status,
            Status::Pass
        );

        for benign in [Reachability::NoListener, Reachability::Unreachable] {
            assert_eq!(
                remote_reachable_check(url, benign, None).status,
                Status::Warn,
                "{benign:?} is a normal local-only state, not a broken install"
            );
        }

        let check = remote_reachable_check(
            url,
            Reachability::NoListener,
            Some(phux_protocol::wire::ListenerDisabledReason::TokenStoreLoadFailed),
        );
        let hint = check.hint.expect("hint");
        assert!(
            hint.contains("restart") || hint.contains("upgrade"),
            "server-disabled wss must name restart, not pair: {hint}"
        );
        assert!(
            !hint.contains("phux pair"),
            "must not blame pairing when the server already explained the disable: {hint}"
        );
    }

    /// Bound-address classification, without dialing.
    #[test]
    fn off_loopback_target_classifies_bound_addresses_without_dialing() {
        let overlay = [IpAddr::V4(std::net::Ipv4Addr::new(100, 64, 0, 2))];

        assert_eq!(
            off_loopback_target(Some("192.0.2.10:9443"), &[]),
            OffLoopbackTarget::Dial("192.0.2.10:9443".parse().unwrap()),
            "a concrete non-loopback bind is probeable without overlay detect"
        );
        assert_eq!(
            off_loopback_target(Some("127.0.0.1:8787"), &overlay),
            OffLoopbackTarget::LoopbackOnly
        );
        assert_eq!(
            off_loopback_target(Some("[::1]:8787"), &overlay),
            OffLoopbackTarget::LoopbackOnly
        );
        assert_eq!(
            off_loopback_target(Some("0.0.0.0:9443"), &overlay),
            OffLoopbackTarget::Dial(SocketAddr::from(([100, 64, 0, 2], 9443))),
            "unspecified bind keeps the bound port, not the default 8787"
        );
        assert!(matches!(
            off_loopback_target(Some("0.0.0.0:8787"), &[]),
            OffLoopbackTarget::NoOffLoopback { .. }
        ));
        assert!(matches!(
            off_loopback_target(None, &overlay),
            OffLoopbackTarget::NoOffLoopback { .. }
        ));
    }

    #[test]
    fn wildcard_probe_selects_only_the_listener_address_family() {
        let v4: IpAddr = "10.77.0.2".parse().unwrap();
        let v6: IpAddr = "fd77::2".parse().unwrap();
        assert_eq!(
            off_loopback_target(Some("[::]:9443"), &[v4, v6]),
            OffLoopbackTarget::Dial(SocketAddr::new(v6, 9443)),
        );
        assert_eq!(
            off_loopback_target(Some("0.0.0.0:9443"), &[v6, v4]),
            OffLoopbackTarget::Dial(SocketAddr::new(v4, 9443)),
        );
        for (bound, other_family) in [("[::]:9443", v4), ("0.0.0.0:9443", v6)] {
            let target = off_loopback_target(Some(bound), &[other_family]);
            assert!(matches!(target, OffLoopbackTarget::NoOffLoopback { .. }));
            let verdict = remote_reachable_verdict(target, |_| {
                unreachable!("family mismatch must warn, not dial another listener")
            });
            assert_eq!(verdict.status, Status::Warn);
        }
    }

    #[test]
    fn loopback_bind_passes_and_unspecified_without_overlay_warns_instead_of_passing() {
        let loopback = remote_reachable_verdict(OffLoopbackTarget::LoopbackOnly, |_| {
            unreachable!("loopback-only must not dial")
        });
        assert_eq!(loopback.status, Status::Pass);

        let unspecified = remote_reachable_verdict(
            OffLoopbackTarget::NoOffLoopback {
                bound: "0.0.0.0:8787".into(),
            },
            |_| unreachable!("no off-loopback target must not dial"),
        );
        assert_eq!(
            unspecified.status,
            Status::Warn,
            "passing hid the firewall stealth-drop when overlay detect was empty"
        );
        assert!(
            unspecified.detail.contains("0.0.0.0:8787"),
            "{}",
            unspecified.detail
        );

        let silent = remote_reachable_verdict(
            OffLoopbackTarget::Dial("192.0.2.10:8787".parse().unwrap()),
            |_| Reachability::Silent,
        );
        assert_eq!(silent.status, Status::Fail);
        assert!(silent.detail.contains("wss://192.0.2.10:8787"));
        assert!(silent.detail.contains("UDS"));
    }

    /// Server-reported disabled listeners fail with a restart hint when the
    /// store now loads.
    #[test]
    fn server_reported_disabled_listeners_fail_with_a_restart_hint_when_the_store_is_fine() {
        use phux_protocol::wire::{
            ListenerDisabledReason, RemoteListenerSlot, RemoteListenerTransport,
            RemoteListenersReport,
        };

        let report = RemoteListenersReport::new().with_listeners(vec![
            RemoteListenerSlot::disabled(
                RemoteListenerTransport::Wss,
                Some("100.64.0.2:8787".into()),
                ListenerDisabledReason::TokenStoreLoadFailed,
            ),
            RemoteListenerSlot::disabled(
                RemoteListenerTransport::Quic,
                Some("100.64.0.2:8788".into()),
                ListenerDisabledReason::TokenStoreLoadFailed,
            ),
        ]);

        let check = remote_listeners_check(Some(&report), true);
        assert_eq!(check.status, Status::Fail);
        assert!(
            check.detail.contains("wss") && check.detail.contains("quic"),
            "detail must name every disabled transport: {}",
            check.detail
        );
        let hint = check.hint.expect("hint");
        assert!(
            hint.contains("loads now") && (hint.contains("restart") || hint.contains("upgrade")),
            "a healthy store against a disabled server must say restart: {hint}"
        );

        let bound = RemoteListenersReport::new().with_listeners(vec![RemoteListenerSlot::bound(
            RemoteListenerTransport::Wss,
            "127.0.0.1:8787",
        )]);
        assert_eq!(
            remote_listeners_check(Some(&bound), true).status,
            Status::Pass
        );
        assert_eq!(remote_listeners_check(None, true).status, Status::Pass);
    }

    /// `workload-authority` fingerprints the CA and reports the registry
    /// generation, never naming the CA private key.
    #[test]
    fn workload_authority_reports_fingerprint_and_generation() {
        let dir = tempfile::tempdir().expect("tempdir");
        let ca = dir.path().join("workload-ca.pem");
        let key = dir.path().join("workload-ca.key");
        let registry = dir.path().join("workload-keys");

        let missing = workload_authority_check(&ca, &registry);
        assert_eq!(missing.status, Status::Pass);
        assert!(
            missing.detail.contains("not initialized"),
            "{}",
            missing.detail
        );

        let status = phux_server::workload::init_authority(&ca, &key).expect("init");
        let fresh = workload_authority_check(&ca, &registry);
        assert_eq!(fresh.status, Status::Pass);
        assert!(
            fresh.detail.contains(&status.fingerprint),
            "{}",
            fresh.detail
        );
        assert!(fresh.detail.contains("generation 0"), "{}", fresh.detail);
        assert!(
            !fresh.detail.contains("workload-ca.key"),
            "{}",
            fresh.detail
        );

        // World-readable: refused, as the server would refuse it.
        std::fs::write(&registry, "{}").expect("write registry");
        let broken = workload_authority_check(&ca, &registry);
        assert_eq!(broken.status, Status::Fail);
        assert!(
            !broken.detail.contains("workload-ca.key"),
            "{}",
            broken.detail
        );
    }

    /// Every `remote-cert` branch says something actionable, and doctor never
    /// touches the certificate.
    #[test]
    fn remote_cert_reports_coverage_without_ever_repairing_it() {
        use phux_server::transport::tls::{cert_fingerprint, ensure_self_signed};

        let dir = tempfile::tempdir().expect("tempdir");
        let cert = dir.path().join("remote-cert.pem");
        let key = dir.path().join("remote-key.pem");
        let overlay = ["100.64.0.2".to_owned()];

        let check = remote_cert_check(&cert, &key, &overlay, false);
        assert_eq!(check.status, Status::Warn);
        assert!(check.hint.expect("hint").contains("phux pair"));

        ensure_self_signed(&cert, &key).expect("provision");
        let fingerprint = cert_fingerprint(&cert).expect("fingerprint");

        let check = remote_cert_check(&cert, &key, &overlay, false);
        assert_eq!(
            check.status,
            Status::Warn,
            "pinning consumers still work, so this is not a failed install"
        );
        assert!(check.detail.contains("100.64.0.2"));
        let hint = check.hint.expect("hint");
        assert!(hint.contains("un-pairing every paired device"), "{hint}");
        assert!(hint.contains(&cert.display().to_string()), "{hint}");
        assert!(hint.contains(&key.display().to_string()), "{hint}");

        let hint = remote_cert_check(&cert, &key, &overlay, true)
            .hint
            .expect("hint");
        assert!(hint.contains("subjectAltName"), "{hint}");
        assert!(!hint.contains("rm "), "{hint}");

        assert_eq!(
            remote_cert_check(&cert, &key, &[], false).status,
            Status::Warn
        );

        let wide_cert = dir.path().join("wide-cert.pem");
        let wide_key = dir.path().join("wide-key.pem");
        phux_server::transport::tls::ensure_self_signed_for(&wide_cert, &wide_key, &overlay)
            .expect("provision wide");
        let check = remote_cert_check(&wide_cert, &wide_key, &overlay, false);
        assert_eq!(check.status, Status::Pass);
        assert!(check.hint.is_none(), "a pass has nothing to remedy");

        assert_eq!(cert_fingerprint(&cert).expect("fingerprint"), fingerprint);
    }

    /// An over-long socket path fails with a hint.
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

    /// A stopped server warns rather than fails.
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

    /// A stale shim warns with both schema numbers and the reinstall command.
    #[test]
    fn a_stale_claude_shim_warns_and_names_the_reinstall_command() {
        let path = std::path::Path::new("/data/phux/shims/claude");
        let check = shim_check(Some(1), 3, path);
        assert_eq!(check.status, Status::Warn);
        assert!(check.detail.contains("schema 1"), "{}", check.detail);
        assert!(check.detail.contains("schema 3"), "{}", check.detail);
        assert!(check.detail.contains("/data/phux/shims/claude"));
        let hint = check
            .hint
            .expect("a warn without a hint is half a diagnosis");
        assert!(
            hint.contains("phux agent install-claude"),
            "the remedy must be the literal command: {hint}"
        );
        assert!(hint.contains("detector"), "{hint}");
        assert!(
            shim_check(Some(2), 3, path)
                .hint
                .expect("hint")
                .contains("agent wait"),
            "schema 2's consequence is the per-turn clobber, not the stand-down",
        );
    }

    /// A newer shim warns with the opposite remedy; absent or current passes.
    #[test]
    fn a_shim_newer_than_the_binary_warns_with_the_other_remedy() {
        let check = shim_check(Some(4), 3, std::path::Path::new("/data/phux/shims/claude"));
        assert_eq!(check.status, Status::Warn);
        let hint = check.hint.expect("hint");
        assert!(hint.contains("phux update"), "{hint}");

        for installed in [None, Some(3)] {
            let check = shim_check(
                installed,
                3,
                std::path::Path::new("/data/phux/shims/claude"),
            );
            assert_eq!(check.status, Status::Pass, "{installed:?}");
            assert!(check.hint.is_none());
        }
    }

    /// The logs line names the server log, counts client logs, names the dir.
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

    /// A fresh machine's absent logs are normal.
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

    /// An unwritable state dir warns with a hint.
    #[cfg(unix)]
    #[test]
    #[allow(clippy::unwrap_used, reason = "test code")]
    fn an_unwritable_state_dir_warns_with_a_hint() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o555)).unwrap();

        let check = check_logs_at(dir.path(), &dir.path().join("server.log"));

        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755)).unwrap();

        assert_eq!(check.status, Status::Warn);
        assert!(check.detail.contains("not writable"));
        let hint = check
            .hint
            .expect("a warn without a hint is half a diagnosis");
        assert!(hint.contains(&dir.path().display().to_string()));
    }

    /// Co-occurring server-health conditions are all reported, and the pass
    /// fallback count is not read.
    #[test]
    fn every_applicable_server_health_condition_is_reported() {
        let unit = std::path::Path::new("/home/u/.config/systemd/user/phux.service");
        let checks = server_health_checks(
            Some((9, 60)),
            Some(unit),
            None,
            Some(("0.13.0", "0.14.0")),
            || panic!("recent_starts must not be read once another condition already applies"),
        );

        assert_eq!(checks.len(), 3, "{checks:?}");
        assert_eq!(checks[0].status, Status::Fail);
        assert!(
            checks[0].detail.contains("crash-looping"),
            "{}",
            checks[0].detail
        );
        assert_eq!(checks[1].status, Status::Warn);
        assert!(
            checks[1].detail.contains(&unit.display().to_string()),
            "{}",
            checks[1].detail
        );
        assert_eq!(checks[2].status, Status::Warn);
        assert!(checks[2].detail.contains("0.13.0"), "{}", checks[2].detail);
        assert!(checks[2].detail.contains("0.14.0"), "{}", checks[2].detail);
    }

    /// A clean host gets exactly one pass line.
    #[test]
    fn server_health_passes_when_nothing_applies() {
        let checks = server_health_checks(None, None, None, None, || 2);
        assert_eq!(checks.len(), 1);
        assert_eq!(checks[0].status, Status::Pass);
        assert!(checks[0].detail.contains("2 server start(s)"));
    }

    /// Armed supervision is a warning carrying the shared explanation verbatim.
    #[test]
    fn armed_supervision_is_surfaced_as_informational() {
        let unit = std::path::Path::new("/home/u/Library/LaunchAgents/com.phux.server.plist");
        let checks = server_health_checks(None, None, Some(unit), None, || {
            panic!("recent_starts must not be read once another condition already applies")
        });

        assert_eq!(checks.len(), 1, "{checks:?}");
        assert_eq!(checks[0].status, Status::Warn);
        assert!(checks[0].detail.contains("armed"), "{}", checks[0].detail);
        assert!(
            checks[0].detail.contains(&unit.display().to_string()),
            "{}",
            checks[0].detail
        );
        let hint = checks[0]
            .hint
            .as_ref()
            .expect("a warn without a hint is half a diagnosis");
        assert!(
            hint.contains("--adopt") && hint.contains("working as designed"),
            "the hint must name the state's origin and that it is intended: {hint}"
        );
        assert!(
            hint.contains("not caught by anything"),
            "the one real risk of the armed window must be stated: {hint}"
        );
        assert!(
            hint.contains("service uninstall"),
            "the way out must be named: {hint}"
        );
        assert!(
            hint.contains(super::super::service::ARMED_SUPERVISION_EXPLANATION),
            "the hint must carry the shared explanation verbatim, not a second copy: {hint}"
        );
    }

    /// The legacy-unit hint names the non-destructive `reconcile`, not
    /// `install`, and says the macOS fix lands at next login.
    #[test]
    fn legacy_unit_hint_stays_honest_about_its_own_remedy() {
        let unit = std::path::Path::new("/home/u/Library/LaunchAgents/com.phux.server.plist");
        let checks = server_health_checks(None, Some(unit), None, None, || 0);
        let hint = checks[0]
            .hint
            .as_ref()
            .expect("a warn without a hint is half a diagnosis");
        assert!(hint.contains("service reconcile"), "{hint}");
        assert!(
            !hint.contains("service install"),
            "the hint must not send a user at the pane-killing remedy now that \
             a non-destructive one exists (phux-nvi2, phux-l1yx): {hint}"
        );
        assert!(
            hint.contains("no pane is lost"),
            "the remedy's zero cost is the reason it replaced the old one; say it: {hint}"
        );
        assert!(
            hint.contains("next login"),
            "on macOS the policy is not in force until then, and a hint that \
             implies otherwise is the same dishonesty nvi2 was filed for: {hint}"
        );
    }

    /// A `ServicePlan` for feeding the real `service` renderers.
    fn service_plan_fixture() -> crate::commands::service::ServicePlan {
        crate::commands::service::ServicePlan {
            binary: PathBuf::from("/usr/local/bin/phux"),
            quic: None,
            listen: None,
            tokens: PathBuf::from("/home/u/.local/state/phux/remote-tokens"),
            cert: PathBuf::from("/home/u/.local/state/phux/remote-cert.pem"),
            key: PathBuf::from("/home/u/.local/state/phux/remote-key.pem"),
            socket: None,
            hub: false,
            socket_path: PathBuf::from("/tmp/phux.sock"),
            profile: None,
            log: PathBuf::from("/home/u/.local/state/phux/server.log"),
            restore: None,
            wrapper: PathBuf::from("/home/u/.local/state/phux/service-wrapper.sh"),
        }
    }

    /// A unit doctor cannot read is not guessed legacy.
    #[test]
    fn an_unreadable_unit_is_not_flagged_legacy() {
        let path = std::path::Path::new("/nonexistent/phux-doctor-test/unit-file");
        assert!(!supervisor_unit_is_legacy(path));
    }

    /// The legacy verdict for fresh units, the pre-throttle shape, a zero
    /// throttle, and `Restart=always` beside a stray `SuccessfulExit`, and its
    /// agreement with `reconcile_unit` on each.
    #[test]
    #[allow(clippy::unwrap_used, reason = "test code")]
    fn supervisor_unit_is_legacy_and_reconcile_unit_agree_on_known_cases() {
        use crate::commands::service::{Manager, Reconcile, reconcile_unit};

        let dir = tempfile::tempdir().unwrap();
        let cases: [(&str, Manager, String, bool); 5] = [
            (
                "fresh-launchd",
                Manager::Launchd,
                crate::commands::service::render_launchd_plist(&service_plan_fixture()),
                false,
            ),
            (
                "fresh-systemd",
                Manager::Systemd,
                crate::commands::service::render_systemd_unit(&service_plan_fixture()),
                false,
            ),
            (
                "pre-zomb4-launchd",
                Manager::Launchd,
                "<?xml version=\"1.0\"?>\n<plist version=\"1.0\">\n<dict>\n  \
                 <key>Label</key>\n  <string>com.phux.server</string>\n  \
                 <key>KeepAlive</key>\n  <true/>\n</dict>\n</plist>\n"
                    .to_owned(),
                true,
            ),
            (
                "zero-throttle-launchd",
                Manager::Launchd,
                "<plist version=\"1.0\">\n<dict>\n  <key>SuccessfulExit</key>\n    <false/>\n  \
                 <key>ThrottleInterval</key>\n  <integer>0</integer>\n</dict>\n</plist>\n"
                    .to_owned(),
                true,
            ),
            (
                "restart-always-systemd",
                Manager::Systemd,
                "[Service]\n# SuccessfulExit is launchd's spelling, not used here\n\
                 Restart=always\nRestartSec=30s\n"
                    .to_owned(),
                true,
            ),
        ];

        for (name, manager, body, expected) in cases {
            let path = dir.path().join(name);
            std::fs::write(&path, &body).unwrap();

            let legacy = supervisor_unit_is_legacy(&path);
            assert_eq!(legacy, expected, "{name}: legacy verdict");
            let would_change = !matches!(reconcile_unit(manager, &body), Reconcile::Current);
            assert_eq!(
                legacy,
                would_change,
                "{name}: supervisor_unit_is_legacy={legacy} but reconcile_unit \
                 {}Current",
                if would_change { "!= " } else { "== " }
            );
        }
    }

    /// The accepted divergence: a retuned positive throttle is not legacy, while
    /// `reconcile_unit` still converges it.
    #[test]
    #[allow(clippy::unwrap_used, reason = "test code")]
    fn a_retuned_but_positive_throttle_is_not_legacy_though_reconcile_would_still_rewrite_it() {
        use crate::commands::service::{Manager, Reconcile, reconcile_unit};

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("com.phux.server.plist");
        let body = crate::commands::service::render_launchd_plist(&service_plan_fixture());
        let current = body
            .split_once("<key>ThrottleInterval</key>")
            .and_then(|(_, rest)| plist_integer(rest))
            .expect("today's renderer always emits a positive ThrottleInterval");
        let retuned = body.replacen(
            &format!("<integer>{current}</integer>"),
            &format!("<integer>{}</integer>", current + 1),
            1,
        );
        assert_ne!(
            retuned, body,
            "the substitution must actually change something"
        );
        std::fs::write(&path, &retuned).unwrap();

        assert!(
            !supervisor_unit_is_legacy(&path),
            "a positive throttle is safe regardless of its exact number"
        );
        assert_ne!(
            reconcile_unit(Manager::Launchd, &retuned),
            Reconcile::Current,
            "reconcile still wants to converge on the exact current throttle value"
        );
    }

    /// A self-signed certificate whose `notAfter` is `days` from now, as
    /// PEM in `dir`; the doctor reads only its validity.
    fn cert_expiring_in(dir: &std::path::Path, file: &str, days: i64) -> PathBuf {
        let key = rcgen::KeyPair::generate().unwrap();
        let mut params = rcgen::CertificateParams::new(vec!["client".to_owned()]).unwrap();
        let at = chrono::Utc::now() + chrono::Duration::days(days);
        params.not_after = rcgen::date_time_ymd(
            chrono::Datelike::year(&at),
            u8::try_from(chrono::Datelike::month(&at)).unwrap(),
            u8::try_from(chrono::Datelike::day(&at)).unwrap(),
        );
        let path = dir.join(file);
        std::fs::write(&path, params.self_signed(&key).unwrap().pem()).unwrap();
        path
    }

    fn remote(
        name: &str,
        cert: Option<PathBuf>,
        key: Option<&str>,
    ) -> crate::commands::remote::RemoteEntry {
        crate::commands::remote::RemoteEntry {
            index: 0,
            name: name.to_owned(),
            endpoint: format!("quic://{name}:8788"),
            token_file: None,
            cert_fingerprint: None,
            tls_server_name: None,
            session: None,
            ssh: Some(format!("me@{name}")),
            direct: None,
            client_cert: cert,
            client_key: key.map(PathBuf::from),
        }
    }

    /// Client certificates pass while good for more than the renewal
    /// window, and one inside it, expired, unreadable, or half-named warns
    /// with the command that fixes that remote.
    #[test]
    fn client_certificates_due_for_renewal_warn_with_the_remedy() {
        let dir = tempfile::tempdir().unwrap();
        let now = chrono::Utc::now().timestamp();
        let fresh = cert_expiring_in(dir.path(), "fresh.pem", 80);
        let soon = cert_expiring_in(dir.path(), "soon.pem", 5);
        let gone = cert_expiring_in(dir.path(), "gone.pem", -3);

        let none = client_certs_check(&[remote("bare", None, None)], now);
        assert_eq!(none.status, Status::Pass, "{none:?}");

        let good = client_certs_check(&[remote("mini", Some(fresh.clone()), Some("/k"))], now);
        assert_eq!(good.status, Status::Pass, "{good:?}");
        assert!(good.detail.contains("first expires"), "{good:?}");

        let due = client_certs_check(
            &[
                remote("mini", Some(fresh), Some("/k")),
                remote("soon", Some(soon), Some("/k")),
                remote("gone", Some(gone), Some("/k")),
                remote("lost", Some(dir.path().join("missing.pem")), Some("/k")),
                remote("half", None, Some("/k")),
            ],
            now,
        );
        assert_eq!(due.status, Status::Warn, "{due:?}");
        assert!(due.detail.contains("soon: expires on"), "{due:?}");
        assert!(due.detail.contains("gone: expired on"), "{due:?}");
        assert!(due.detail.contains("lost: at"), "{due:?}");
        assert!(due.detail.contains("half: names only one"), "{due:?}");
        assert!(!due.detail.contains("mini"), "{due:?}");
        let hint = due.hint.unwrap_or_default();
        for remedy in [
            "phux host renew soon",
            "phux host renew gone",
            "phux host renew lost",
            "phux host add me@half",
        ] {
            assert!(hint.contains(remedy), "{hint}");
        }
    }

    /// A check name longer than the old fixed 12-column field still lines
    /// its detail (and its hint arrow) up with every other row.
    #[test]
    fn human_report_aligns_details_past_the_longest_name() {
        let checks = [
            Check::pass("config", "fine"),
            Check::warn("workload-authority", "missing", "init it"),
        ];
        let rendered = render_human(&checks);
        let lines: Vec<&str> = rendered.lines().collect();
        let detail_column = |line: &str, needle: &str| line.find(needle).expect(needle);
        assert_eq!(
            detail_column(lines[0], "fine"),
            detail_column(lines[1], "missing"),
            "{rendered}"
        );
        assert_eq!(
            detail_column(lines[2], "->"),
            detail_column(lines[1], "missing"),
            "{rendered}"
        );
    }
}
