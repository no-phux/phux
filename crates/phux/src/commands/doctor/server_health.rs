//! Server health: crash loops, stale builds, and supervisor units.

use std::path::PathBuf;

use super::Check;

/// Is the server crash-looping, running a stale build, under a legacy
/// (unthrottled) supervisor, or armed but not yet supervised? A restarting
/// server is otherwise indistinguishable from a healthy one (ADR-0080).
/// Every applicable condition is reported; they tend to co-occur.
pub(super) fn check_server_health(socket_path: &std::path::Path) -> Vec<Check> {
    let unit = legacy_service_unit_path().filter(|path| path.exists());
    let legacy_unit = unit
        .as_deref()
        .filter(|unit| supervisor_unit_is_legacy(unit));

    let crash_loop = phux_server::health::crash_loop()
        .map(|count| (count, phux_server::health::CRASH_LOOP_WINDOW.as_secs() / 60));

    // Armed-but-inactive supervision (ADR-0088), scoped to this socket.
    let armed_unit = crate::commands::service::armed_adoption_unit(socket_path);

    // Version skew: the binary was replaced but the server was not restarted.
    let ours = env!("CARGO_PKG_VERSION");
    let theirs = phux_server::health::running_version();
    let version_skew = theirs
        .as_deref()
        .filter(|theirs| *theirs != ours)
        .map(|theirs| (theirs, ours));

    let incompatible = version_skew.is_some() && incompatible_running_server(socket_path);
    let pid = incompatible
        .then(|| crate::commands::diagnostics::server_pid(socket_path))
        .flatten();
    server_health_checks(
        crash_loop,
        legacy_unit,
        armed_unit.as_deref(),
        version_skew,
        incompatible.then_some((socket_path, pid)),
        || phux_server::health::recent_starts(phux_server::health::CRASH_LOOP_WINDOW).len(),
    )
}

/// Whether the live server at `socket` refused this binary's protocol.
fn incompatible_running_server(socket: &std::path::Path) -> bool {
    if !socket.exists() {
        return false;
    }
    let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    else {
        return false;
    };
    runtime
        .block_on(phux_client::state::get_state(socket))
        .is_err_and(|err| phux_client::attach::is_incompatible_protocol(&err))
}

/// The pure half of [`check_server_health`]. `recent_starts` is only read for
/// the pass fallback.
pub(super) fn server_health_checks(
    crash_loop: Option<(usize, u64)>,
    legacy_unit: Option<&std::path::Path>,
    armed_unit: Option<&std::path::Path>,
    version_skew: Option<(&str, &str)>,
    recovery: Option<(&std::path::Path, Option<i32>)>,
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
                crate::commands::service::ARMED_SUPERVISION_EXPLANATION
            ),
        ));
    }

    if let Some((theirs, ours)) = version_skew {
        checks.push(Check::warn(
            "server-health",
            format!("the running server is {theirs}; this binary is {ours}"),
            version_skew_hint(theirs, ours, recovery),
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

/// Choose the remedy by release ordering. Checkout builds that cannot be
/// ordered retain auto-attach's replaced-binary hand-over policy.
///
/// `recovery` is set when the live server refused this protocol. That case
/// cannot hand over in place, so the hint is the stop-and-start recovery.
fn version_skew_hint(
    theirs: &str,
    ours: &str,
    recovery: Option<(&std::path::Path, Option<i32>)>,
) -> String {
    use phux_client_runtime::release::Version;

    if let Some((socket, pid)) = recovery {
        return crate::commands::diagnostics::old_server_recovery_lines(socket, pid).join("\n");
    }

    match (Version::parse(theirs), Version::parse(ours)) {
        (Some(theirs), Some(ours)) if theirs > ours => {
            "run `phux update` to update this binary; leave the newer server running".to_owned()
        }
        _ => "run `phux upgrade` to hand the server over in place (panes survive), \
             or attach with `phux`, which now does it automatically"
            .to_owned(),
    }
}

/// Whether `unit`'s restart behavior is dangerous: not failure-only, or not
/// throttled to a positive interval. Checks values, not key presence.
///
/// Deliberately looser than [`crate::commands::service::reconcile_unit`]'s
/// byte-exact "current" test, so retuning the throttle constant does not make
/// every previously installed (still safe) unit warn. The tests pin where the
/// two predicates agree and the one case where they may not.
pub(super) fn supervisor_unit_is_legacy(unit: &std::path::Path) -> bool {
    let Ok(body) = std::fs::read_to_string(unit) else {
        return false;
    };
    !(restart_is_failure_only(&body) && restart_is_throttled(&body))
}

/// Does `body` restart only on abnormal exit (launchd `SuccessfulExit` false,
/// or systemd exactly `Restart=on-failure`)?
pub(super) fn restart_is_failure_only(body: &str) -> bool {
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
pub(super) fn restart_is_throttled(body: &str) -> bool {
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
pub(super) fn plist_integer(rest_after_key: &str) -> Option<u64> {
    let (_, rest) = rest_after_key.split_once("<integer>")?;
    let (digits, _) = rest.split_once("</integer>")?;
    digits.trim().parse().ok()
}

/// The next whitespace-delimited token after a systemd `Key=`.
pub(super) fn value_token(rest: &str) -> &str {
    let end = rest.find(|c: char| c.is_whitespace()).unwrap_or(rest.len());
    rest[..end].trim()
}

/// The leading ASCII digits of `value` (reads `30s` or `30`).
pub(super) fn leading_digits(value: &str) -> Option<u64> {
    let digits: String = value.chars().take_while(char::is_ascii_digit).collect();
    if digits.is_empty() {
        None
    } else {
        digits.parse().ok()
    }
}

/// Where `service install` writes this profile's unit, if this platform has
/// a generator and `HOME` is set.
pub(super) fn legacy_service_unit_path() -> Option<PathBuf> {
    let manager = crate::commands::service::Manager::host()?;
    manager
        .unit_path(crate::commands::service::profile_suffix().as_deref())
        .ok()
}

#[cfg(test)]
mod tests;
