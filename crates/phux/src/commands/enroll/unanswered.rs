//! Hosts whose UDP went unanswered. A QUIC probe that times out is
//! remembered for a day, so the next attach goes straight to ssh instead of
//! waiting out another one; a probe that connects forgets the host.

use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// How long an unanswered probe stands.
const REMEMBER: Duration = Duration::from_hours(24);

fn file() -> PathBuf {
    phux_config::instance::state_dir().join("udp-unanswered")
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

/// Whether a probe to `route` (`HOST`, `HOST:PORT`, `[V6]:PORT` or
/// `quic://…`) timed out within the last day without one connecting since.
pub(crate) fn udp_unanswered(route: &str) -> bool {
    let text = std::fs::read_to_string(file()).unwrap_or_default();
    remembered(&text, host_of(route), now())
}

/// Note what a probe to `route` found: `answered`, or no answer at all.
/// Best effort: a state directory that cannot be written costs a wait.
pub(super) fn record(route: &str, answered: bool) {
    let path = file();
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    let next = updated(&text, host_of(route), answered, now());
    if next != text {
        let _ = path.parent().map(std::fs::create_dir_all);
        let _ = std::fs::write(&path, next);
    }
}

/// The host in `route`, without scheme, port or IPv6 brackets.
fn host_of(route: &str) -> &str {
    let route = route.strip_prefix("quic://").unwrap_or(route);
    if let Some(v6) = route.strip_prefix('[') {
        return v6.split_once(']').map_or(v6, |(host, _)| host);
    }
    match route.split_once(':') {
        Some((host, port)) if !port.contains(':') => host,
        _ => route,
    }
}

/// Each line is `UNIX_SECONDS HOST`; lines past [`REMEMBER`] are dropped.
fn fresh(text: &str, now: u64) -> impl Iterator<Item = (u64, &str)> {
    text.lines().filter_map(move |line| {
        let (at, host) = line.split_once(' ')?;
        let at = at.parse::<u64>().ok()?;
        (now.saturating_sub(at) < REMEMBER.as_secs()).then_some((at, host))
    })
}

fn remembered(text: &str, host: &str, now: u64) -> bool {
    fresh(text, now).any(|(_, seen)| seen == host)
}

fn updated(text: &str, host: &str, answered: bool, now: u64) -> String {
    let mut lines: Vec<String> = fresh(text, now)
        .filter(|(_, seen)| *seen != host)
        .map(|(at, seen)| format!("{at} {seen}\n"))
        .collect();
    if !answered {
        lines.push(format!("{now} {host}\n"));
    }
    lines.concat()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_timed_out_probe_is_remembered_for_a_day_and_an_answer_forgets_it() {
        let day = REMEMBER.as_secs();
        let text = updated("", "box", false, 1_000);
        assert!(remembered(&text, "box", 1_000 + day - 1));
        assert!(!remembered(&text, "box", 1_000 + day), "expired");
        assert!(!remembered(&text, "other", 1_000));

        let both = updated(&text, "other", false, 2_000);
        assert!(remembered(&both, "box", 2_000) && remembered(&both, "other", 2_000));
        let answered = updated(&both, "box", true, 3_000);
        assert!(!remembered(&answered, "box", 3_000));
        assert!(remembered(&answered, "other", 3_000));

        // A fresh timeout restarts the day; expired lines are dropped.
        let again = updated(&text, "box", false, 1_000 + day / 2);
        assert!(remembered(&again, "box", 1_000 + day + 1));
        assert_eq!(updated(&text, "x", true, 1_000 + day), "");
        assert!(!remembered("garbage\n12 \n", "box", 12));
    }

    #[test]
    fn a_route_is_remembered_by_its_host() {
        for (route, host) in [
            ("box", "box"),
            ("box:8788", "box"),
            ("quic://box.tail.net:8788", "box.tail.net"),
            ("[fd7a::1]:8788", "fd7a::1"),
            ("quic://[fd7a::1]:8788", "fd7a::1"),
            ("fd7a::1", "fd7a::1"),
            ("10.0.0.5:60123", "10.0.0.5"),
        ] {
            assert_eq!(host_of(route), host, "{route}");
        }
    }
}
