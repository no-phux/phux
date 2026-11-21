//! Whether this host recently failed to route a reply to a remote peer.
//!
//! Listeners and the local reachability probe can both pass while the
//! server still cannot send. That is a route problem, not credentials.

use std::path::Path;

use phux_server::runtime::reply_route::{ReplyRouteFailure, read_recent};

use super::Check;

const NAME: &str = "reply-route";
const MAX_AGE_MS: u64 = 15 * 60 * 1000;
const HINT: &str = "listeners being up does not prove this host can route replies to that peer. \
     Check the route (Tailscale or the LAN). This is not an auth or certificate failure.";

/// The `reply-route` check. Warnings do not fail doctor.
pub(super) fn check_reply_route() -> Check {
    reply_route_from_dir(&phux_config::instance::state_dir(), unix_now_ms())
}

fn reply_route_from_dir(dir: &Path, now_ms: u64) -> Check {
    let failures = read_recent(dir, now_ms, MAX_AGE_MS);
    reply_route_verdict(&failures, now_ms)
}

fn reply_route_verdict(failures: &[ReplyRouteFailure], now_ms: u64) -> Check {
    let recent = recent(failures, now_ms);
    if recent.is_empty() {
        return Check::pass(NAME, "no recent reply routing failures");
    }
    Check::warn(NAME, detail(&recent), HINT)
}

fn recent(failures: &[ReplyRouteFailure], now_ms: u64) -> Vec<&ReplyRouteFailure> {
    failures
        .iter()
        .filter(|failure| now_ms.saturating_sub(failure.at_ms) <= MAX_AGE_MS)
        .collect()
}

fn detail(failures: &[&ReplyRouteFailure]) -> String {
    let listed = failures
        .iter()
        .map(|failure| format!("{} ({})", failure.peer, failure.kind))
        .collect::<Vec<_>>()
        .join("; ");
    format!("replies could not be routed to {listed}")
}

fn unix_now_ms() -> u64 {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis());
    u64::try_from(millis).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::{MAX_AGE_MS, reply_route_from_dir, reply_route_verdict};
    use crate::commands::doctor::Status;
    use phux_server::runtime::reply_route::ReplyRouteFailure;

    fn failure(peer: &str, kind: &str, at_ms: u64) -> ReplyRouteFailure {
        ReplyRouteFailure {
            peer: peer.to_owned(),
            kind: kind.to_owned(),
            at_ms,
        }
    }

    #[test]
    fn stale_reply_route_failures_pass() {
        let now = 2_000_000_u64;
        let stale = failure("100.64.0.2:4242", "host_unreachable", now - MAX_AGE_MS - 1);
        let check = reply_route_verdict(&[stale], now);
        assert_eq!(check.status, Status::Pass);
        assert_eq!(check.name, "reply-route");
        assert!(check.detail.contains("no recent reply routing failures"));
        assert!(check.hint.is_none());
    }

    #[test]
    fn fresh_reply_route_failure_warns_without_blaming_auth() {
        let now = 2_000_000_u64;
        let fresh = failure("100.64.0.2:4242", "host_unreachable", now - 1_000);
        let stale = failure("100.64.0.9:9", "host_down", now - MAX_AGE_MS - 1);
        let check = reply_route_verdict(&[stale, fresh], now);
        assert_eq!(check.status, Status::Warn);
        assert!(check.detail.contains("100.64.0.2:4242"), "{}", check.detail);
        assert!(
            check.detail.contains("host_unreachable"),
            "{}",
            check.detail
        );
        assert!(
            !check.detail.contains("100.64.0.9"),
            "a stale peer is not a current failure: {}",
            check.detail
        );
        let hint = check.hint.expect("a warning names what to check");
        let lower = hint.to_lowercase();
        assert!(
            lower.contains("not an auth or certificate failure"),
            "hint must exclude credentials as the cause: {hint}"
        );
        assert!(lower.contains("route"), "{hint}");
        assert!(!lower.contains("token"), "hint blames a token: {hint}");
        assert!(!lower.contains("re-pair"), "hint blames pairing: {hint}");
        assert!(
            !lower.contains("unauthorized"),
            "hint blames authorization: {hint}"
        );
    }

    #[test]
    fn reply_route_file_passes_when_only_stale_and_warns_when_fresh() {
        let dir = tempfile::tempdir().expect("temp");
        let now = 2_000_000_u64;
        let fresh_at = now - 1_000;
        let stale_at = now - MAX_AGE_MS - 1;
        let body = format!(
            r#"{{"version":1,"failures":[{{"peer":"100.64.0.8:9","kind":"host_unreachable","at_ms":{fresh_at}}},{{"peer":"100.64.0.9:9","kind":"host_down","at_ms":{stale_at}}}]}}"#
        );
        std::fs::write(dir.path().join("reply-route.json"), body).expect("write");

        let fresh = reply_route_from_dir(dir.path(), now);
        assert_eq!(fresh.status, Status::Warn);
        assert!(fresh.detail.contains("100.64.0.8:9"), "{}", fresh.detail);
        assert!(!fresh.detail.contains("100.64.0.9"), "{}", fresh.detail);

        let empty = tempfile::tempdir().expect("temp");
        let none = reply_route_from_dir(empty.path(), now);
        assert_eq!(none.status, Status::Pass);
        assert!(none.detail.contains("no recent reply routing failures"));
    }
}
