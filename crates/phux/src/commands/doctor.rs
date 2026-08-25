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

/// `phux doctor [--json] [--socket PATH]`.
///
/// Exit codes: 0 when nothing failed (warnings do not fail the run — a
/// stopped server is a normal state, not a broken install), 1 when any check
/// failed.
pub(crate) fn run_doctor(json: bool, socket: Option<PathBuf>) -> ExitCode {
    let socket_path = socket.unwrap_or_else(default_socket_path);
    let mut checks = vec![
        check_config(),
        check_instance(),
        check_socket_path(&socket_path),
        check_server(&socket_path),
    ];
    // Not a single `Check`: several server-health conditions can hold at
    // once, and per phux-67wg they co-occur more than they don't. See
    // `check_server_health`'s doc comment.
    checks.extend(check_server_health(&socket_path));
    checks.extend([
        check_plugins(),
        check_agent_shim(),
        check_remote_cert(),
        check_remote_reachable(),
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

/// Which instance is this binary talking to, and why (phux-zomb.2)?
///
/// Earns its place because the isolation is *automatic*: a developer running
/// a `target/` build gets a different socket, state directory, and session
/// list than their installed phux, and the only symptom of not realising it
/// is "my sessions are gone". Naming the profile and the reason it was chosen
/// turns that into a one-line answer.
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
    // A warning, not a failure: an isolated instance is working as designed.
    // It is surfaced at all because the isolation is silent by construction.
    Check::warn(
        "instance",
        format!("profile {profile} ({reason}); state {}", state.display()),
        "this instance is isolated from your installed phux — its sessions and \
         logs are separate. Unset PHUX_PROFILE, or run the installed binary, to \
         reach the default instance",
    )
}

/// Is the server crash-looping, is it running a stale build, and is its
/// supervisor the pre-phux-zomb.4 kind that hides both?
///
/// This is the check whose absence let a broken server pass for a working one
/// for weeks. A supervised server that dies and restarts is externally
/// indistinguishable from one that never fell over — the socket answers
/// either way. Counting restarts is the only way the difference becomes
/// visible without reading a log (ADR-0080).
///
/// Gathers the three signals and hands them to [`server_health_checks`],
/// which reports every one that applies (phux-dsg1) — a crash-looping host
/// with a legacy unit is not a corner case: per phux-67wg, a legacy unit's
/// unthrottled restarts are exactly what produces a crash-loop, so the two
/// conditions co-occur precisely when hearing about only one is least
/// useful.
fn check_server_health(socket_path: &std::path::Path) -> Vec<Check> {
    let unit = legacy_service_unit_path().filter(|path| path.exists());
    let legacy_unit = unit
        .as_deref()
        .filter(|unit| supervisor_unit_is_legacy(unit));

    let crash_loop = phux_server::health::crash_loop()
        .map(|count| (count, phux_server::health::CRASH_LOOP_WINDOW.as_secs() / 60));

    // Supervision armed by `--adopt` but not yet active (ADR-0088): the unit
    // is written and deliberately unloaded while the incumbent server runs.
    // Scoped to the socket being diagnosed — a marker armed against another
    // profile or another `--socket` override is not this instance's state,
    // and reporting it here would describe a server that is not the one the
    // rest of this run is about.
    let armed_unit = super::service::armed_adoption_unit(socket_path);

    // Version skew: a package manager replaced the binary but nothing
    // restarted the server, so it is still serving the old build (phux-zomb.7).
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

/// The pure half of [`check_server_health`]: turns already-gathered signals
/// into every applicable [`Check`], instead of the first one found. Split out
/// so the co-occurrence behavior itself is testable without a running
/// server, a real supervisor unit on disk, or a mutated environment — same
/// idiom as [`shim_check`] beside [`check_agent_shim`].
///
/// `recent_starts` is a closure, not a value, so the Pass-fallback count is
/// read only when nothing else already applies — matching the original
/// early-return version, which never paid for that read once a fail or warn
/// fired first.
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

    // A unit generated before phux-zomb.4 restarts on *every* exit with no
    // throttle. `phux service reconcile` replaces its restart-policy keys with
    // the throttled, failure-only ones, which is both the fix and the way a
    // crash-loop stays visible.
    if let Some(unit) = legacy_unit {
        checks.push(Check::warn(
            "server-health",
            format!(
                "the supervisor unit at {} restarts on every exit, unthrottled",
                unit.display()
            ),
            // This hint used to name `phux service install`, which reloads the
            // unit -- and the reload boots out the running job, so following
            // it ended every pane and its in-flight shells, agents, and
            // subagents. A Warn exits 0 and reads as routine housekeeping,
            // which is exactly when an unannounced destructive step does the
            // most damage (phux-nvi2). `reconcile` (phux-l1yx) rewrites the
            // policy in place and stops nothing, so the hint can now point at
            // a remedy whose cost is zero -- and say where it is not yet in
            // force, rather than leaving macOS users to assume it is.
            "it resurrects servers you stopped and hides crash-loops — run \
             `phux service reconcile` to correct it in place; nothing is \
             stopped and no pane is lost (on macOS the corrected policy takes \
             effect at your next login, which that command spells out)",
        ));
    }

    // Supervision that is armed but not yet active (ADR-0088, phux-8514).
    // Working as designed — and surfaced for the same reason ADR-0080
    // surfaced the crash-loop: an invisible supervision state is how a
    // broken server passes for a working one, and until the hand-over the
    // running server is not restart-managed by anything.
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

/// Whether `unit` was generated before the restart policy was corrected.
///
/// Compares actual VALUES against what [`crate::commands::service`]'s
/// renderers write, not just whether the keys that carry them appear
/// anywhere in the file. A unit that merely contains the tokens
/// `ThrottleInterval` / `RestartSec` / `SuccessfulExit` / `Restart=on-failure`
/// — with a zero throttle, a `Restart=always` policy, or a stray match inside
/// an unrelated line — is not the corrected policy; only the values the
/// generator actually writes are. A unit missing them entirely predates
/// phux-zomb.4 (or was hand-edited into the same unthrottled shape), and
/// either way deserves the same warning.
///
/// ## Why this stays a second predicate next to `service::reconcile_unit` (phux-x2k8)
///
/// `service::Reconcile::Current` ([`crate::commands::service::reconcile_unit`])
/// answers a byte-exact question: would patching this file with today's
/// [`crate::commands::service::launchd_policy_lines`] /
/// [`crate::commands::service::systemd_policy_lines`] change anything? That is
/// the right question for the reconciler — it has to be a fixed point
/// (`reconcile(reconcile(x)) == reconcile(x)`) and its whole job is
/// converging a unit onto this build's exact canonical bytes.
///
/// A diagnostic needs a looser question: is this unit's restart behavior
/// dangerous — unthrottled, or restarting on a clean exit — regardless of
/// which build wrote it? Keying the warning to byte-exact match would make it
/// fire on a plain constant retune (say, a future release changing the
/// throttle interval) even though the installed unit is still perfectly
/// safe: throttled, failure-only, just pinned to an older number. A `Warn`
/// that starts firing on every such release trains people to ignore it,
/// which is the failure mode `server-health` staying a trustworthy exit-0
/// warning (phux-nvi2) exists to avoid. So this predicate checks value shape
/// (failure-only, throttle greater than zero) rather than byte identity, and
/// is deliberately allowed to disagree with `reconcile_unit` on that one
/// case.
///
/// They are required to agree on every case that has actually occurred in
/// practice — a freshly generated unit, the pre-zomb.4 shape, and dsg1's two
/// false-pass fixtures — which
/// `supervisor_unit_is_legacy_and_reconcile_unit_agree_on_known_cases` (below)
/// pins directly against both functions. The one case they must NOT agree on
/// is pinned by
/// `a_retuned_but_positive_throttle_is_not_legacy_though_reconcile_would_still_rewrite_it`.
/// Both predicates independently stay cross-checked against the real
/// renderers, so neither can silently drift from what `phux service install`
/// actually writes.
fn supervisor_unit_is_legacy(unit: &std::path::Path) -> bool {
    let Ok(body) = std::fs::read_to_string(unit) else {
        return false;
    };
    !(restart_is_failure_only(&body) && restart_is_throttled(&body))
}

/// Does `body` restart only on abnormal exit — launchd's
/// `<key>SuccessfulExit</key><false/>`, or systemd's exact `Restart=on-failure`
/// (never `Restart=always`, which still contains the bare substring
/// `Restart=` the old check keyed on)?
fn restart_is_failure_only(body: &str) -> bool {
    if let Some((_, rest)) = body.split_once("<key>SuccessfulExit</key>") {
        return rest.trim_start().starts_with("<false/>");
    }
    if let Some((_, rest)) = body.split_once("Restart=") {
        return value_token(rest) == "on-failure";
    }
    false
}

/// Does `body` throttle restarts to a positive interval — launchd's
/// `<key>ThrottleInterval</key><integer>N</integer>`, or systemd's
/// `RestartSec=Ns` — with `N` greater than zero in both cases? `N == 0` wears
/// the corrected key over the legacy (unthrottled) behavior.
fn restart_is_throttled(body: &str) -> bool {
    if let Some((_, rest)) = body.split_once("<key>ThrottleInterval</key>") {
        return plist_integer(rest).is_some_and(|n| n > 0);
    }
    if let Some((_, rest)) = body.split_once("RestartSec=") {
        return leading_digits(value_token(rest)).is_some_and(|n| n > 0);
    }
    false
}

/// The `<integer>N</integer>` immediately following a plist key's closing
/// tag, exactly as [`crate::commands::service::render_launchd_plist`] emits
/// it: `rest` starts right after `</key>`.
fn plist_integer(rest_after_key: &str) -> Option<u64> {
    let (_, rest) = rest_after_key.split_once("<integer>")?;
    let (digits, _) = rest.split_once("</integer>")?;
    digits.trim().parse().ok()
}

/// The next whitespace-delimited token in `rest`, which starts right after a
/// systemd `Key=` marker.
fn value_token(rest: &str) -> &str {
    let end = rest.find(|c: char| c.is_whitespace()).unwrap_or(rest.len());
    rest[..end].trim()
}

/// The leading run of ASCII digits in `value`, parsed as an integer — enough
/// to read a systemd time span like `30s`, or a bare `30`.
fn leading_digits(value: &str) -> Option<u64> {
    let digits: String = value.chars().take_while(char::is_ascii_digit).collect();
    if digits.is_empty() {
        None
    } else {
        digits.parse().ok()
    }
}

/// Which init system a legacy unit would have been written for. Mirrors
/// `service::Manager`, kept as its own type (rather than importing that one)
/// for the same reason [`legacy_service_unit_path`] duplicates its path
/// logic instead of calling it.
///
/// The allow sits on the *type*, not on one variant, and that is the whole
/// subtlety: `host()` constructs exactly one of these per target, so which
/// variant is dead depends on which platform is compiling. Allowing only
/// `Systemd` passed on macOS and failed CI on Linux with "variant `Launchd`
/// is never constructed". Both stay constructible from tests on every
/// platform, which is the point — `legacy_service_unit_path_for` must be
/// driveable for both managers regardless of which host runs the suite.
#[allow(
    dead_code,
    reason = "host() constructs one variant per target; the other is reached \
              only from tests, and which one that is flips with the platform"
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LegacyManager {
    Launchd,
    Systemd,
}

impl LegacyManager {
    /// The manager for the host we were built for. `None` on a platform with
    /// neither — there is no legacy unit to look for there.
    #[allow(
        clippy::unnecessary_wraps,
        reason = "None is reachable on targets that are neither macOS nor Linux; clippy only sees the active cfg"
    )]
    const fn host() -> Option<Self> {
        #[cfg(target_os = "macos")]
        {
            Some(Self::Launchd)
        }
        #[cfg(target_os = "linux")]
        {
            Some(Self::Systemd)
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        {
            None
        }
    }
}

/// Where `service install` writes its unit.
///
/// Duplicated from the `service` module's private path logic rather than
/// exposed from it, because doctor must keep reporting the location even for
/// units this build would no longer generate. That reasoning still holds,
/// but the duplication now has to track more than a fixed pair of paths:
/// `service::launchd_label_for` / `service::systemd_unit_for` profile-scope
/// every unit but the default profile's (phux-gyza), and this mirrors that
/// scoping. Without it, doctor would silently stop finding a legacy unit on
/// any non-default profile — exactly the profile-aware detection ADR-0080
/// exists to give.
fn legacy_service_unit_path() -> Option<PathBuf> {
    legacy_service_unit_path_for(
        LegacyManager::host()?,
        std::env::var_os("HOME"),
        std::env::var_os("XDG_CONFIG_HOME"),
        legacy_profile_suffix().as_deref(),
    )
}

/// The active ADR-0080 profile when it is not the default, else `None`. Same
/// rule as `service::profile_suffix`, duplicated for the same reason as
/// [`legacy_service_unit_path`].
fn legacy_profile_suffix() -> Option<String> {
    (!phux_config::instance::is_default_profile()).then(phux_config::instance::profile)
}

/// [`legacy_service_unit_path`] with every input injectable: which manager,
/// `HOME`, `XDG_CONFIG_HOME`, and the active profile. Lets a test drive both
/// managers and either profile from one platform, and an unset `HOME`,
/// without mutating the process environment (`env::set_var` is unsafe under
/// edition 2024 and this crate forbids unsafe code) — same idiom as
/// `service::home_dir_from`.
fn legacy_service_unit_path_for(
    manager: LegacyManager,
    home: Option<std::ffi::OsString>,
    xdg_config_home: Option<std::ffi::OsString>,
    profile: Option<&str>,
) -> Option<PathBuf> {
    let home = PathBuf::from(home.filter(|value| !value.is_empty())?);
    match manager {
        LegacyManager::Launchd => {
            let label = profile.map_or_else(
                || "com.phux.server".to_owned(),
                |profile| format!("com.phux.server.{profile}"),
            );
            Some(
                home.join("Library")
                    .join("LaunchAgents")
                    .join(format!("{label}.plist")),
            )
        }
        LegacyManager::Systemd => {
            let config = xdg_config_home
                .filter(|value| !value.is_empty())
                .map_or_else(|| home.join(".config"), PathBuf::from);
            let unit = profile.map_or_else(
                || "phux.service".to_owned(),
                |profile| format!("phux-{profile}.service"),
            );
            Some(config.join("systemd").join("user").join(unit))
        }
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
                     with `phux host ls --role satellite`",
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

/// Is the installed Claude shim the one this binary knows how to write?
///
/// This check exists because the shim is **not part of the binary**
/// (phux-w7z2.46). It is a `/bin/sh` script written once into a phux-owned
/// directory, and it keeps running whatever text it was written with, so
/// upgrading phux does not upgrade an installed shim. Someone who upgrades and
/// never re-runs `phux agent install-claude` keeps last release's behavior
/// indefinitely with nothing anywhere saying so.
///
/// That would be a cosmetic complaint if the versions were cosmetic, and they
/// are not: a schema-1 shim declares an agent `state` on every Claude hook,
/// which per `docs/spec/L3.md` §3.7 outranks the server's derivation for the
/// life of the record — so on that machine the ADR-0046 detector is stood down
/// on every Claude pane, and a `SIGKILL`ed Claude keeps a `working` badge. The
/// bug is fixed in the binary and still live on disk. A silent, install-time
/// mismatch with a one-command remedy is precisely doctor's remit.
///
/// `Warn`, never `Fail`: phux works fine, and doctor's exit code gates setup
/// scripts that have nothing to do with Claude.
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

/// The pure half of [`check_agent_shim`]: compare what is on disk against what
/// this binary writes. Split out so every branch is testable without an
/// installed shim or a mutated environment.
fn shim_check(installed: Option<u32>, current: u32, path: &std::path::Path) -> Check {
    let where_ = path.display();
    match installed {
        // Never installed is a normal state, not a missing precondition:
        // `install-claude` is opt-in and most users never run it.
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
        // Newer on disk than this binary writes: a downgraded or older phux.
        // Still a mismatch worth naming, and the remedy differs.
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

/// What the user is actually living with, per stale schema.
///
/// Deliberately concrete: "your shim is old" is not a diagnosis, and these two
/// versions fail in different, individually recognizable ways. The wording
/// tracks the `install-claude` upgrade notice so the two never disagree.
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
        _ => "the installed shim predates this binary's wrapper behavior",
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

/// Does the remote-consumer certificate name the address phux advertises?
///
/// This check exists because the answer is fixed at generation time and can
/// never be corrected in place (phux-q9a0, ADR-0091). SANs are chosen when the
/// certificate is minted; widening them means a new certificate, which means a
/// new SHA-256 fingerprint, which un-pairs every device that pinned the old
/// one. So a certificate generated before phux learned to name the overlay
/// address stays narrow for as long as it exists, and nothing else on the
/// system would ever say so — the server keeps working, `phux pair` keeps
/// printing a link, and only a third-party client that validates the server
/// name ever sees the mismatch. That silent, install-time, one-command-remedy
/// shape is exactly doctor's remit.
///
/// `Warn`, never `Fail`: every phux consumer pins the fingerprint and ignores
/// the name (`phux-dial`'s `CertTrust`, and phux-mobile's verifier), so the
/// documented pairing flow works end to end on a narrow certificate. Calling
/// that a failure would turn doctor's exit code red on installs where nothing
/// is broken.
/// How long the reachability probe waits for the listener to say anything.
///
/// Generous relative to a loopback-speed handshake, because the probe rides
/// whatever overlay the operator uses; short enough that `phux doctor` stays
/// an interactive command when the answer is "blocked".
const REMOTE_PROBE_TIMEOUT: Duration = Duration::from_secs(4);

/// What dialing our own routable listener revealed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Reachability {
    /// The listener answered. A completed handshake and a refusal at the auth
    /// layer both land here on purpose: either way packets reached phux, which
    /// is the only thing this check is asking.
    Answered,
    /// The connection was refused, so nothing is bound there.
    NoListener,
    /// The connection was accepted and then nothing came back. This is the
    /// shape a host packet filter produces: the kernel completes the TCP
    /// handshake, so the dial "connects", but the bytes never reach the
    /// process and the dial hangs until it times out.
    Silent,
    /// The address could not be reached at all.
    Unreachable,
}

/// Does traffic to the routable listener actually reach the server?
///
/// Every other check here reads local state — a bound socket, a parsed
/// config, a cert on disk — and local state is exactly what stays healthy
/// when a host firewall is dropping inbound packets. A server can be running,
/// listening, correctly paired, and completely unreachable, and before this
/// check `phux doctor` reported that server as entirely fine.
///
/// So this one leaves the machine: it dials its own advertised address with
/// the same stack a real client uses, and reports what came back. Skipped
/// when there is no overlay address to dial, which is the common local-only
/// case and not a fault.
fn check_remote_reachable() -> Check {
    let advertised = phux_config::overlay::detect();
    let Some(addr) = advertised.first().copied() else {
        return Check::pass(
            "remote-reachable",
            "no overlay address detected; nothing routable to probe",
        );
    };
    let url = format!(
        "wss://{}:{}",
        phux_server::transport::tls::san_name(addr),
        phux_server::runtime::DEFAULT_WS_PORT
    );
    remote_reachable_check(&url, probe_remote_listener(&url))
}

/// Dial `url` and classify the answer.
///
/// Trust is [`CertTrust::SkipVerify`] and no token is sent: this is a
/// reachability probe, not an auth check. A 401 from the upgrade is a
/// perfectly good answer — it proves the packets landed.
fn probe_remote_listener(url: &str) -> Reachability {
    let Ok(runtime) = cli_runtime() else {
        return Reachability::Unreachable;
    };
    let dial = phux_dial::ws::WsDial {
        url: url.to_owned(),
        token: None,
        trust: phux_dial::CertTrust::SkipVerify,
        tls_server_name: None,
    };
    let probe = async {
        let Ok(outcome) =
            tokio::time::timeout(REMOTE_PROBE_TIMEOUT, phux_dial::ws::dial(&dial)).await
        else {
            // Nothing came back inside the window. The connection was
            // accepted and then went nowhere — see [`Reachability::Silent`].
            return Reachability::Silent;
        };
        match outcome {
            // `Unreachable` is the dial's own "refused / no route / network
            // down" bucket, and only a refusal proves the address itself is
            // fine with nothing bound behind it.
            Err(phux_dial::DialError::Unreachable(err)) => {
                if err.to_lowercase().contains("refused") {
                    Reachability::NoListener
                } else {
                    Reachability::Unreachable
                }
            }
            // Everything else — a completed handshake, a TLS failure, an
            // auth refusal — means something answered, which is the only
            // question this check is asking.
            Ok(_) | Err(_) => Reachability::Answered,
        }
    };
    runtime.block_on(probe)
}

/// The pure half of [`check_remote_reachable`], so every verdict is testable
/// without a tailnet, a listener, or a firewall.
fn remote_reachable_check(url: &str, reachability: Reachability) -> Check {
    match reachability {
        Reachability::Answered => Check::pass(
            "remote-reachable",
            format!("{url} answered; remote clients can reach this server"),
        ),
        Reachability::Silent => Check::fail(
            "remote-reachable",
            format!(
                "{url} accepted a connection and then answered nothing — the socket is \
                 bound but traffic is not reaching phux, which is what a host firewall \
                 looks like from here"
            ),
            FIREWALL_REMEDY,
        ),
        Reachability::NoListener => Check::warn(
            "remote-reachable",
            format!("nothing is listening on {url}"),
            "expected remote access? run `phux pair` — the listener only auto-binds \
             once a device credential exists",
        ),
        Reachability::Unreachable => Check::warn(
            "remote-reachable",
            format!("could not reach {url} from this host"),
            "if the address belongs to an overlay network, check it is up: `tailscale status`",
        ),
    }
}

/// What to do about a listener that is bound but unreachable.
///
/// macOS gets named specifically because it is the case operators cannot
/// guess: the application firewall drops inbound connections to a binary it
/// does not recognize, phux ships adhoc-signed so it is never recognized, and
/// an allowlist entry is keyed to the exact binary path — so upgrading phux
/// silently breaks remote access even for someone who allowlisted it once.
#[cfg(target_os = "macos")]
const FIREWALL_REMEDY: &str = "macOS: the application firewall blocks inbound connections to \
     unrecognized binaries, and phux is adhoc-signed. Check it with \
     `/usr/libexec/ApplicationFirewall/socketfilterfw --getglobalstate`. Allowlisting is \
     per-binary-path, so it breaks again on the next upgrade; turning the firewall off on a \
     host that lives behind an overlay network is the durable fix";

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
    // Same source of truth `phux pair` and the auto-listener use (ADR-0037).
    // Doctor is an interactive diagnostic, so paying for the detection
    // shell-out here is fine — it is the startup path that must not.
    let advertised: Vec<String> = phux_config::overlay::detect()
        .into_iter()
        .map(phux_server::transport::tls::san_name)
        .collect();
    remote_cert_check(&cert, &key, &advertised, operator_cert)
}

/// The pure half of [`check_remote_cert`], so every branch is testable against
/// a temp dir without a tailnet or a mutated environment.
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
            "nothing to do unless you expected an overlay: check `tailscale ip -4`",
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
        // A `--json` path, so even this last-resort failure is the shared
        // contract line, never prose — `doctor --json` keeps stderr free of
        // unstructured text on every failure exit (phux-i0e8.8.3).
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

    /// A bound-but-unreachable listener is the one failure every other check
    /// here reports as healthy, so this check has to fail loudly and say what
    /// to do. The silent case is the whole reason it exists: a host firewall
    /// lets the kernel finish the TCP handshake and then eats the bytes, so
    /// the server looks perfect from the inside while no client can reach it.
    #[test]
    fn a_bound_but_silent_listener_fails_with_an_actionable_remedy() {
        let url = "wss://100.64.0.2:8787";

        let check = remote_reachable_check(url, Reachability::Silent);
        assert_eq!(
            check.status,
            Status::Fail,
            "a listener that answers nothing is a failure, not a warning: \
             remote access is entirely broken"
        );
        let hint = check.hint.expect("a failure must carry a remedy");
        assert!(
            hint.contains("firewall") || hint.contains("packet filter"),
            "the remedy must name the blocker: {hint}"
        );

        // An answer of any kind proves packets land, which is all this asks —
        // an auth refusal counts, so a probe that sends no token still passes.
        assert_eq!(
            remote_reachable_check(url, Reachability::Answered).status,
            Status::Pass
        );

        // Not having paired, and having no overlay, are ordinary states.
        // Neither may fail the run and strand someone with exit 1.
        for benign in [Reachability::NoListener, Reachability::Unreachable] {
            assert_eq!(
                remote_reachable_check(url, benign).status,
                Status::Warn,
                "{benign:?} is a normal local-only state, not a broken install"
            );
        }
    }

    /// The `remote-cert` check is the durable surface for a certificate that
    /// cannot be corrected in place (phux-q9a0, ADR-0091), so every branch
    /// has to say something a user can act on.
    #[test]
    fn remote_cert_reports_coverage_without_ever_repairing_it() {
        use phux_server::transport::tls::{cert_fingerprint, ensure_self_signed};

        let dir = tempfile::tempdir().expect("tempdir");
        let cert = dir.path().join("remote-cert.pem");
        let key = dir.path().join("remote-key.pem");
        let overlay = ["100.64.0.2".to_owned()];

        // Nothing provisioned yet: a warning that names the one command that
        // fixes it, never a failure (not having paired is a normal state).
        let check = remote_cert_check(&cert, &key, &overlay, false);
        assert_eq!(check.status, Status::Warn);
        assert!(check.hint.expect("hint").contains("phux pair"));

        ensure_self_signed(&cert, &key).expect("provision");
        let fingerprint = cert_fingerprint(&cert).expect("fingerprint");

        // A narrow certificate warns and hands over the exact remediation,
        // including the fact that it un-pairs devices.
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

        // An operator-supplied certificate gets a remedy aimed at their CA,
        // not an `rm` of a file phux does not own.
        let hint = remote_cert_check(&cert, &key, &overlay, true)
            .hint
            .expect("hint");
        assert!(hint.contains("subjectAltName"), "{hint}");
        assert!(!hint.contains("rm "), "{hint}");

        // Nothing detected: not checkable, therefore not a pass.
        assert_eq!(
            remote_cert_check(&cert, &key, &[], false).status,
            Status::Warn
        );

        // A certificate that does name the address passes.
        let wide_cert = dir.path().join("wide-cert.pem");
        let wide_key = dir.path().join("wide-key.pem");
        phux_server::transport::tls::ensure_self_signed_for(&wide_cert, &wide_key, &overlay)
            .expect("provision wide");
        let check = remote_cert_check(&wide_cert, &wide_key, &overlay, false);
        assert_eq!(check.status, Status::Pass);
        assert!(check.hint.is_none(), "a pass has nothing to remedy");

        // Doctor mutates nothing: the narrow certificate is byte-identical,
        // and so is the fingerprint every paired device pinned.
        assert_eq!(cert_fingerprint(&cert).expect("fingerprint"), fingerprint);
    }

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

    /// phux-w7z2.46, the case this check was added for: the binary moved on
    /// and the shim on disk did not. The line must say both numbers and name
    /// the exact command that fixes it — the user has no way to guess that
    /// re-running the installer is what closes a gap nothing else reports.
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
        // Each stale schema fails in its own recognizable way, and the hint
        // says which one the user is living with.
        assert!(hint.contains("detector"), "{hint}");
        assert!(
            shim_check(Some(2), 3, path)
                .hint
                .expect("hint")
                .contains("agent wait"),
            "schema 2's consequence is the per-turn clobber, not the stand-down",
        );
    }

    /// A machine that is already current gets no warning — the whole point of
    /// a staleness check is that it stays quiet when nothing is stale.
    #[test]
    fn a_current_claude_shim_does_not_warn() {
        let check = shim_check(Some(3), 3, std::path::Path::new("/data/phux/shims/claude"));
        assert_eq!(check.status, Status::Pass);
        assert!(check.hint.is_none());
    }

    /// `install-claude` is opt-in, so "never installed" is a normal state and
    /// must not read as a problem on the many machines that never run it.
    #[test]
    fn an_absent_claude_shim_is_not_a_problem() {
        let check = shim_check(None, 3, std::path::Path::new("/data/phux/shims/claude"));
        assert_eq!(check.status, Status::Pass);
        assert!(check.detail.contains("no claude-in-phux shim installed"));
    }

    /// The mismatch runs both ways: an older binary against a newer shim is
    /// still a mismatch, and its remedy is the opposite one.
    #[test]
    fn a_shim_newer_than_the_binary_warns_with_the_other_remedy() {
        let check = shim_check(Some(4), 3, std::path::Path::new("/data/phux/shims/claude"));
        assert_eq!(check.status, Status::Warn);
        let hint = check.hint.expect("hint");
        assert!(hint.contains("phux update"), "{hint}");
    }

    /// A stale shim must never fail the run: phux itself works, and doctor's
    /// exit code gates setup scripts that have nothing to do with Claude.
    #[test]
    fn a_stale_shim_never_fails_the_run() {
        let checks = vec![shim_check(
            Some(1),
            3,
            std::path::Path::new("/data/phux/shims/claude"),
        )];
        assert_eq!(report_human(&checks), ExitCode::SUCCESS);
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

    // -----------------------------------------------------------------
    // server-health: co-occurrence (phux-dsg1)
    // -----------------------------------------------------------------

    /// phux-dsg1's headline defect: crash-loop and a legacy supervisor unit
    /// co-occur precisely when the old early-return version most needed to
    /// report both — per phux-67wg, a legacy unit's unthrottled restarts are
    /// exactly what produces a crash-loop. Breaking this back into "only the
    /// first condition is reported" means a user with two problems hears
    /// about one. The `recent_starts` closure panics if called, pinning that
    /// the Pass-fallback count is never read once something else applies.
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

    /// A clean host still gets exactly one report — the pre-phux-dsg1 shape
    /// when nothing applies must survive the rewrite unchanged.
    #[test]
    fn server_health_passes_when_nothing_applies() {
        let checks = server_health_checks(None, None, None, None, || 2);
        assert_eq!(checks.len(), 1);
        assert_eq!(checks[0].status, Status::Pass);
        assert!(checks[0].detail.contains("2 server start(s)"));
    }

    /// phux-8514's adjacent gap: an armed-but-not-active supervision unit
    /// (ADR-0088) was invisible to doctor, which reported all checks passing
    /// while the running server was not restart-managed by anything — the
    /// same invisible-supervision-state shape ADR-0080 made the crash-loop
    /// reportable for. Informational: a Warn, not a Fail, because armed is
    /// working as designed; it displaces the Pass line so the in-between
    /// state is named rather than summarised away.
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
        // The explanation itself has one home (phux-8514 wrote it twice, in
        // two crates' worth of wording that had already drifted). Doctor
        // frames it; it does not restate it.
        assert!(
            hint.contains(super::super::service::ARMED_SUPERVISION_EXPLANATION),
            "the hint must carry the shared explanation verbatim, not a second copy: {hint}"
        );
    }

    /// phux-nvi2: the legacy-unit hint must stay honest about what following
    /// it actually does. A `Warn` exits 0 and reads as routine housekeeping,
    /// which is exactly when an unannounced destructive step does the most
    /// damage.
    ///
    /// The guard survives phux-l1yx, but its *premise* moved and the
    /// assertions moved with it. The remedy used to be `phux service install`,
    /// which reloads the unit and therefore ends every pane, so the hint had
    /// to say so. It is now `phux service reconcile`, which rewrites the
    /// policy keys in place and stops nothing — so the honest hint no longer
    /// warns about pane loss, because there is none to warn about.
    ///
    /// What nvi2 actually guarantees is unchanged and is what is asserted
    /// here: the hint names its remedy, that remedy is not the destructive
    /// one, and any way in which following it falls short of a complete fix
    /// is stated rather than left to be discovered. On macOS the corrected
    /// policy cannot take effect without a `bootout`, so "not in force until
    /// next login" is exactly that kind of shortfall.
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

    // -----------------------------------------------------------------
    // supervisor_unit_is_legacy: values, not key presence (phux-dsg1)
    // -----------------------------------------------------------------

    /// A `ServicePlan` with every field populated, for feeding the real
    /// `service` renderers. Ties the legacy-detection tests to what
    /// `service install` actually writes today, rather than a hand-typed
    /// fixture that could quietly drift from it.
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

    /// Whatever `service install` writes today must never trip doctor's
    /// legacy warning. This is the test the bug report says was missing
    /// entirely: the detection logic is the half of ADR-0080 that runs on
    /// users' machines, while the rendering it detects already had 17 tests.
    #[test]
    #[allow(clippy::unwrap_used, reason = "test code")]
    fn a_freshly_generated_launchd_unit_is_never_flagged_legacy() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("com.phux.server.plist");
        let plist = crate::commands::service::render_launchd_plist(&service_plan_fixture());
        std::fs::write(&path, plist).unwrap();

        assert!(!supervisor_unit_is_legacy(&path));
    }

    /// The systemd half of the same guarantee.
    #[test]
    #[allow(clippy::unwrap_used, reason = "test code")]
    fn a_freshly_generated_systemd_unit_is_never_flagged_legacy() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("phux.service");
        let unit = crate::commands::service::render_systemd_unit(&service_plan_fixture());
        std::fs::write(&path, unit).unwrap();

        assert!(!supervisor_unit_is_legacy(&path));
    }

    /// The actual pre-phux-zomb.4 shape: `KeepAlive` as a bare boolean, no
    /// `SuccessfulExit` or `ThrottleInterval` keys at all. This is the unit
    /// every host that has never re-run `phux service install` since is
    /// still running.
    #[test]
    #[allow(clippy::unwrap_used, reason = "test code")]
    fn a_pre_zomb4_launchd_unit_is_flagged_legacy() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("com.phux.server.plist");
        std::fs::write(
            &path,
            "<?xml version=\"1.0\"?>\n<plist><dict>\n  \
             <key>Label</key>\n  <string>com.phux.server</string>\n  \
             <key>KeepAlive</key>\n  <true/>\n</dict></plist>\n",
        )
        .unwrap();

        assert!(supervisor_unit_is_legacy(&path));
    }

    /// phux-dsg1's cited false pass: a zero throttle wears the corrected key
    /// but keeps the legacy (unthrottled) behavior. The old substring-only
    /// check could not tell the difference between this and a real 30s
    /// throttle.
    #[test]
    #[allow(clippy::unwrap_used, reason = "test code")]
    fn a_zero_throttle_interval_is_still_legacy() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("com.phux.server.plist");
        std::fs::write(
            &path,
            "<plist><dict>\n  <key>SuccessfulExit</key>\n    <false/>\n  \
             <key>ThrottleInterval</key>\n  <integer>0</integer>\n</dict></plist>\n",
        )
        .unwrap();

        assert!(
            supervisor_unit_is_legacy(&path),
            "a zero throttle is the legacy behavior wearing the corrected key"
        );
    }

    /// phux-dsg1's other cited false pass: `Restart=always` beside a stray
    /// mention of `SuccessfulExit` (e.g. in a comment) used to read as legacy
    /// because the old check only asked whether each word appeared anywhere
    /// in the file, never what value it carried.
    #[test]
    #[allow(clippy::unwrap_used, reason = "test code")]
    fn restart_always_is_legacy_despite_a_stray_successfulexit_mention() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("phux.service");
        std::fs::write(
            &path,
            "[Service]\n# SuccessfulExit is launchd's spelling, not used here\n\
             Restart=always\nRestartSec=30s\n",
        )
        .unwrap();

        assert!(
            supervisor_unit_is_legacy(&path),
            "`Restart=always` is the legacy policy regardless of what other tokens appear in the file"
        );
    }

    /// A unit doctor cannot read (removed mid-check, permissions, etc.) must
    /// not be guessed broken — `check_server_health` already filters to
    /// existing paths, but this pins the function's own defensive behavior.
    #[test]
    fn an_unreadable_unit_is_not_flagged_legacy() {
        let path = std::path::Path::new("/nonexistent/phux-doctor-test/unit-file");
        assert!(!supervisor_unit_is_legacy(path));
    }

    // -----------------------------------------------------------------
    // supervisor_unit_is_legacy vs service::reconcile_unit (phux-x2k8)
    // -----------------------------------------------------------------
    //
    // Two predicates now answer a related question about the same unit
    // files (doctor's "should I warn" and service's "would reconciling
    // change anything"). The design decision — keep both, because they
    // answer genuinely different questions — is documented on
    // `supervisor_unit_is_legacy` above. These two tests are what makes that
    // decision durable instead of just prose: one pins where they must
    // agree, the other pins the one place they are allowed not to.

    /// Every case that has actually mattered in practice — a freshly
    /// generated unit for both managers, the real pre-zomb.4 shape, and
    /// dsg1's two specific false-pass fixtures (a zero throttle, and
    /// `Restart=always` beside a stray `SuccessfulExit` mention) — must get
    /// the same legacy verdict from `supervisor_unit_is_legacy` as from
    /// `reconcile_unit`'s `Current`/not-`Current` split. If a future change
    /// made these disagree on any of these cases, that is a real
    /// regression, not the accepted divergence the next test pins.
    #[test]
    #[allow(clippy::unwrap_used, reason = "test code")]
    fn supervisor_unit_is_legacy_and_reconcile_unit_agree_on_known_cases() {
        use crate::commands::service::{Manager, Reconcile, reconcile_unit};

        let dir = tempfile::tempdir().unwrap();
        let cases: [(&str, Manager, String); 5] = [
            (
                "fresh-launchd",
                Manager::Launchd,
                crate::commands::service::render_launchd_plist(&service_plan_fixture()),
            ),
            (
                "fresh-systemd",
                Manager::Systemd,
                crate::commands::service::render_systemd_unit(&service_plan_fixture()),
            ),
            (
                "pre-zomb4-launchd",
                Manager::Launchd,
                "<?xml version=\"1.0\"?>\n<plist version=\"1.0\">\n<dict>\n  \
                 <key>Label</key>\n  <string>com.phux.server</string>\n  \
                 <key>KeepAlive</key>\n  <true/>\n</dict>\n</plist>\n"
                    .to_owned(),
            ),
            (
                "zero-throttle-launchd",
                Manager::Launchd,
                "<plist version=\"1.0\">\n<dict>\n  <key>SuccessfulExit</key>\n    <false/>\n  \
                 <key>ThrottleInterval</key>\n  <integer>0</integer>\n</dict>\n</plist>\n"
                    .to_owned(),
            ),
            (
                "restart-always-systemd",
                Manager::Systemd,
                "[Service]\n# SuccessfulExit is launchd's spelling, not used here\n\
                 Restart=always\nRestartSec=30s\n"
                    .to_owned(),
            ),
        ];

        for (name, manager, body) in cases {
            let path = dir.path().join(name);
            std::fs::write(&path, &body).unwrap();

            let legacy = supervisor_unit_is_legacy(&path);
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

    /// The accepted divergence: a throttle that is a real, positive number
    /// but not *today's exact* number is not dangerous, so doctor does not
    /// warn about it — while `reconcile_unit` still wants to rewrite it,
    /// because its job is convergence onto the current canonical bytes, not
    /// a judgment about safety. If `RESTART_THROTTLE_SECS` is ever retuned,
    /// a unit installed by the previous release must not start producing a
    /// doctor warning it did not produce before the upgrade — that is the
    /// concrete reason the two predicates are not one.
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

    // -----------------------------------------------------------------
    // legacy_service_unit_path: profile scoping (phux-dsg1)
    // -----------------------------------------------------------------

    /// The default profile keeps the unscoped name, matching
    /// `service::launchd_label_for`/`unit_path` for the default profile.
    #[test]
    fn legacy_unit_path_default_profile_launchd() {
        let path = legacy_service_unit_path_for(
            LegacyManager::Launchd,
            Some(std::ffi::OsString::from("/Users/u")),
            None,
            None,
        );
        assert_eq!(
            path,
            Some(PathBuf::from(
                "/Users/u/Library/LaunchAgents/com.phux.server.plist"
            ))
        );
    }

    /// phux-gyza / phux-dsg1: a non-default profile's unit is filed under a
    /// suffixed label, not the bare one — `service.rs` profile-scopes every
    /// unit but the default profile's, and this duplicate has to track that
    /// scoping or doctor silently stops finding a legacy unit on any
    /// non-default profile.
    #[test]
    fn legacy_unit_path_scopes_by_profile_launchd() {
        let path = legacy_service_unit_path_for(
            LegacyManager::Launchd,
            Some(std::ffi::OsString::from("/Users/u")),
            None,
            Some("dev"),
        );
        assert_eq!(
            path,
            Some(PathBuf::from(
                "/Users/u/Library/LaunchAgents/com.phux.server.dev.plist"
            ))
        );
    }

    /// The systemd half of the same profile-scoping guarantee.
    #[test]
    fn legacy_unit_path_scopes_by_profile_systemd() {
        let path = legacy_service_unit_path_for(
            LegacyManager::Systemd,
            Some(std::ffi::OsString::from("/home/u")),
            None,
            Some("dev"),
        );
        assert_eq!(
            path,
            Some(PathBuf::from(
                "/home/u/.config/systemd/user/phux-dev.service"
            ))
        );
    }

    /// `XDG_CONFIG_HOME` still overrides the default `~/.config` join on the
    /// systemd side, same as `service::config_home`.
    #[test]
    fn legacy_unit_path_systemd_respects_xdg_config_home() {
        let path = legacy_service_unit_path_for(
            LegacyManager::Systemd,
            Some(std::ffi::OsString::from("/home/u")),
            Some(std::ffi::OsString::from("/custom/config")),
            None,
        );
        assert_eq!(
            path,
            Some(PathBuf::from("/custom/config/systemd/user/phux.service"))
        );
    }

    /// No `HOME` means no path — the naive join used to silently produce a
    /// path relative to the current working directory instead.
    #[test]
    fn legacy_unit_path_none_without_home() {
        assert_eq!(
            legacy_service_unit_path_for(LegacyManager::Launchd, None, None, None),
            None
        );
    }
}
