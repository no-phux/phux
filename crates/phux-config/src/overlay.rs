//! Best-effort overlay-network address detection (ADR-0037).
//!
//! `phux pair` prints credentials but no address, leaving the operator to
//! hunt down the host's overlay IP by hand. This module closes that gap:
//! the primary source is the `tailscale` CLI (`tailscale ip -4`, which is
//! also the Headscale client), overridable via `$PHUX_TAILSCALE` (listed in
//! the CLI ENVIRONMENT help) — mirroring the `$PHUX_SSH` seam in the hub
//! dialer; the fallback is a zero-dependency UDP route probe that reports
//! the kernel-chosen source address only when it sits inside the Tailscale
//! CGNAT range (`100.64.0.0/10`).
//!
//! Per ADR-0037 phux stays overlay-agnostic: nothing here is load-bearing,
//! every failure (missing binary, tailscaled down, unparseable output)
//! degrades to printing nothing, and no overlay is special-cased below the
//! UX layer. Known limitation: raw `WireGuard` or Nebula overlays addressed
//! from ordinary private ranges (`10.x`, `192.168.x`) are not detected —
//! their operators find the address with their usual tooling.

use std::net::IpAddr;

/// Detect the host's overlay-network addresses, best effort.
///
/// Returns an empty vec when nothing is detected — callers print nothing
/// and detection can never affect an exit code.
pub fn detect() -> Vec<IpAddr> {
    detect_with(tailscale_ip_output, cgnat_route_probe)
}

/// [`detect`] with both sources injectable, so tests can drive the
/// tailscale-wins / fallback / nothing-detected matrix without a tailnet.
fn detect_with(
    tailscale: impl Fn() -> Option<String>,
    route_probe: impl Fn() -> Option<IpAddr>,
) -> Vec<IpAddr> {
    let addrs = tailscale()
        .map(|out| parse_tailscale_ip_output(&out))
        .unwrap_or_default();
    if !addrs.is_empty() {
        return addrs;
    }
    route_probe()
        .filter(|ip| is_cgnat(*ip))
        .into_iter()
        .collect()
}

/// Run `tailscale ip -4` (or the `$PHUX_TAILSCALE` override) and return its
/// stdout, or `None` when the binary is missing, exits non-zero (tailscaled
/// down, not logged in), or outlives the deadline (tailscaled wedged).
fn tailscale_ip_output() -> Option<String> {
    let program = std::env::var_os("PHUX_TAILSCALE").unwrap_or_else(|| "tailscale".into());
    run_tailscale_ip(&program)
}

/// How long the tailscale shell-out may run before it is killed. `tailscale
/// ip` answers in milliseconds when healthy; anything slower means a wedged
/// tailscaled, and best-effort detection must never hang `phux pair`.
const TAILSCALE_DEADLINE: std::time::Duration = std::time::Duration::from_secs(2);

/// The spawn half of [`tailscale_ip_output`], with the program injectable
/// so tests can point it at a stub script without mutating the environment
/// (`env::set_var` is unsafe under edition 2024 and this crate forbids
/// unsafe code).
///
/// The wait is bounded by [`TAILSCALE_DEADLINE`]: `tailscale ip` talks to
/// tailscaled over its local API socket and can block indefinitely when the
/// daemon is wedged (mid-upgrade, stuck state). A blocking `output()` call
/// would hang `phux pair` with it, so the child is polled against the
/// deadline and killed on expiry, degrading to the route probe.
fn run_tailscale_ip(program: &std::ffi::OsStr) -> Option<String> {
    run_tailscale_ip_with_deadline(program, TAILSCALE_DEADLINE)
}

/// [`run_tailscale_ip`] with the deadline injectable, so the happy-path
/// round-trip test can use a bound generous enough to survive a loaded CI
/// box (a `/bin/sh` stub can take over 2s just to spawn there) without
/// loosening the production deadline. Deadline *behavior* is pinned
/// separately by the wedged-stub test.
fn run_tailscale_ip_with_deadline(
    program: &std::ffi::OsStr,
    deadline: std::time::Duration,
) -> Option<String> {
    let mut child = std::process::Command::new(program)
        .args(["ip", "-4"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .ok()?;

    let deadline = std::time::Instant::now() + deadline;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(25));
            }
            // Deadline expired (or the wait itself failed): kill and reap
            // so no zombie outlives the command, then report nothing.
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    };
    if !status.success() {
        return None;
    }
    // Reading stdout only after exit cannot deadlock: a couple of seconds
    // of `tailscale ip` output (a handful of address lines) is far below
    // the pipe buffer capacity, so the child never blocks on a full pipe.
    let mut buf = Vec::new();
    std::io::Read::read_to_end(&mut child.stdout.take()?, &mut buf).ok()?;
    Some(String::from_utf8_lossy(&buf).into_owned())
}

/// Parse `tailscale ip` output: one address per line, tolerating v6 lines
/// and garbage, dropping loopback and unspecified addresses.
fn parse_tailscale_ip_output(out: &str) -> Vec<IpAddr> {
    out.lines()
        .filter_map(|line| line.trim().parse::<IpAddr>().ok())
        .filter(|ip| !ip.is_loopback() && !ip.is_unspecified())
        .collect()
}

/// Ask the kernel which source address it would route toward the Tailscale
/// service IP. `connect` on UDP sends no packets; it only selects a route.
fn cgnat_route_probe() -> Option<IpAddr> {
    let sock = std::net::UdpSocket::bind(("0.0.0.0", 0)).ok()?;
    sock.connect(("100.100.100.100", 1)).ok()?;
    sock.local_addr().ok().map(|addr| addr.ip())
}

/// Whether `ip` sits inside the CGNAT range Tailscale assigns from,
/// `100.64.0.0/10`.
fn is_cgnat(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let [a, b, ..] = v4.octets();
            a == 100 && (64..128).contains(&b)
        }
        IpAddr::V6(_) => false,
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().expect("test ip")
    }

    #[test]
    fn parses_mixed_tailscale_output_and_drops_junk() {
        let out = "100.101.102.103\n  fd7a:115c:a1e0::1  \n127.0.0.1\n0.0.0.0\nnot an ip\n";
        assert_eq!(
            parse_tailscale_ip_output(out),
            vec![ip("100.101.102.103"), ip("fd7a:115c:a1e0::1")]
        );
        assert!(parse_tailscale_ip_output("").is_empty());
    }

    #[test]
    fn cgnat_range_boundaries() {
        assert!(is_cgnat(ip("100.64.0.0")));
        assert!(is_cgnat(ip("100.127.255.255")));
        assert!(!is_cgnat(ip("100.63.255.255")));
        assert!(!is_cgnat(ip("100.128.0.0")));
        assert!(!is_cgnat(ip("192.168.1.5")));
        assert!(!is_cgnat(ip("fd7a:115c:a1e0::1")));
    }

    #[test]
    fn tailscale_output_wins_without_consulting_the_probe() {
        let addrs = detect_with(
            || Some("100.99.98.97\n".to_owned()),
            || panic!("probe must not run when tailscale answered"),
        );
        assert_eq!(addrs, vec![ip("100.99.98.97")]);
    }

    #[test]
    fn probe_fallback_is_filtered_to_cgnat() {
        let addrs = detect_with(|| None, || Some(ip("100.101.102.103")));
        assert_eq!(addrs, vec![ip("100.101.102.103")]);

        // A non-CGNAT route (no overlay; the probe picked the LAN
        // interface) must not be reported as an overlay address.
        assert!(detect_with(|| None, || Some(ip("192.168.1.5"))).is_empty());
        assert!(detect_with(|| None, || None).is_empty());
    }

    #[test]
    fn missing_binary_degrades_to_none() {
        assert!(
            run_tailscale_ip(std::ffi::OsStr::new("/nonexistent/phux-no-such-binary")).is_none()
        );
    }

    /// End-to-end through the real spawn path: a stub script standing in
    /// for the tailscale CLI (the `$PHUX_TAILSCALE` seam, injected directly
    /// because env mutation is unsafe under edition 2024).
    #[cfg(unix)]
    #[test]
    fn stub_tailscale_binary_round_trips() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().expect("tempdir");
        let script = dir.path().join("tailscale-stub");
        std::fs::write(&script, "#!/bin/sh\necho 100.99.98.97\n").expect("write stub");
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))
            .expect("chmod stub");

        // Generous deadline: this test pins the spawn/parse round-trip, not
        // deadline behavior (the wedged-stub test does that). Under full
        // parallel nextest load the stub's /bin/sh spawn alone has been
        // observed to blow the 2s production deadline and flake this test.
        let out =
            run_tailscale_ip_with_deadline(script.as_os_str(), std::time::Duration::from_secs(30))
                .expect("stub output");
        assert_eq!(parse_tailscale_ip_output(&out), vec![ip("100.99.98.97")]);
    }

    /// A wedged tailscaled must not hang `phux pair`: a stub that sleeps
    /// far past the deadline is killed and degrades to `None`. The
    /// deadline is injected short (the test waits it out for real, and the
    /// 2s production default is pure dead time — the kill-at-deadline
    /// behavior is what is under test, not the 2s figure). The elapsed
    /// bound is generous (well under the stub's sleep, well over the
    /// deadline) to keep slow CI from flaking.
    #[cfg(unix)]
    #[test]
    fn wedged_tailscale_binary_is_killed_at_the_deadline() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().expect("tempdir");
        let script = dir.path().join("tailscale-wedged");
        std::fs::write(&script, "#!/bin/sh\nsleep 10\necho 100.99.98.97\n").expect("write stub");
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))
            .expect("chmod stub");

        let start = std::time::Instant::now();
        let addrs = detect_with(
            || {
                run_tailscale_ip_with_deadline(
                    script.as_os_str(),
                    std::time::Duration::from_millis(250),
                )
            },
            || None,
        );
        let elapsed = start.elapsed();
        assert!(addrs.is_empty(), "wedged stub must report nothing");
        assert!(
            elapsed < std::time::Duration::from_secs(5),
            "deadline must fire long before the stub exits; took {elapsed:?}"
        );
    }
}
