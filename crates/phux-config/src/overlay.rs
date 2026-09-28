//! Best-effort overlay-network address detection for `phux pair` (ADR-0037).
//!
//! The source is `tailscale ip -4` (overridable via `$PHUX_TAILSCALE`), with a
//! fallback UDP route probe reported only inside the Tailscale CGNAT range.
//! The probe is a guess made only when `$PHUX_TAILSCALE` is unset: a named CLI
//! is the whole answer, even when it says nothing. Every failure degrades to
//! detecting nothing.

use std::net::IpAddr;

/// Detect the host's overlay-network addresses, best effort (empty when
/// nothing is detected).
#[must_use]
pub fn detect() -> Vec<IpAddr> {
    detect_from_override(std::env::var_os("PHUX_TAILSCALE").as_deref())
}

/// [`detect`] with the `$PHUX_TAILSCALE` value injected for tests.
fn detect_from_override(program: Option<&std::ffi::OsStr>) -> Vec<IpAddr> {
    let Some(program) = program else {
        return detect_with(
            || run_tailscale_ip(std::ffi::OsStr::new("tailscale")),
            cgnat_route_probe,
        );
    };
    detect_with(|| run_tailscale_ip(program), || None)
}

/// [`detect`] with both sources injectable.
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

/// How long `tailscale ip` may run before it is killed: a wedged tailscaled
/// must never hang `phux pair`.
const TAILSCALE_DEADLINE: std::time::Duration = std::time::Duration::from_secs(2);

/// Run `<program> ip -4` and return its stdout, or `None` when it is missing,
/// fails, or outlives [`TAILSCALE_DEADLINE`] (it is then killed).
fn run_tailscale_ip(program: &std::ffi::OsStr) -> Option<String> {
    run_tailscale_ip_with_deadline(program, TAILSCALE_DEADLINE)
}

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
            // Deadline expired: kill and reap so no zombie outlives us.
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
    // A few address lines never fill the pipe, so reading after exit is safe.
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

    /// A configured overlay CLI that reports nothing means "no overlay": the
    /// route probe must not second-guess it (it would report the host's
    /// real tailnet address).
    #[test]
    fn a_configured_overlay_cli_suppresses_the_route_probe() {
        assert!(
            detect_from_override(Some(std::ffi::OsStr::new(
                "/nonexistent/phux-no-such-binary"
            )))
            .is_empty(),
            "a configured overlay CLI that cannot answer must not be \
             second-guessed from the routing table",
        );
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

    /// End-to-end through the real spawn path with a stub CLI.
    #[cfg(unix)]
    #[test]
    fn stub_tailscale_binary_round_trips() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().expect("tempdir");
        let script = dir.path().join("tailscale-stub");
        std::fs::write(&script, "#!/bin/sh\necho 100.99.98.97\n").expect("write stub");
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))
            .expect("chmod stub");

        // Generous deadline: a loaded CI box can take seconds to spawn /bin/sh.
        let out =
            run_tailscale_ip_with_deadline(script.as_os_str(), std::time::Duration::from_secs(30))
                .expect("stub output");
        assert_eq!(parse_tailscale_ip_output(&out), vec![ip("100.99.98.97")]);
    }

    /// A wedged tailscaled must not hang `phux pair`: a stub sleeping past
    /// the deadline is killed and degrades to nothing.
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
