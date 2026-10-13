//! Best-effort overlay-network address detection for `phux pair` (ADR-0037).
//!
//! `$PHUX_OVERLAY_ADDRS` explicitly selects addresses for any private network
//! (including Defguard/WireGuard); when set it is authoritative, even empty or
//! invalid. Otherwise the source is `tailscale ip -4` (overridable via
//! `$PHUX_TAILSCALE`), with a fallback UDP route probe reported only inside the
//! Tailscale CGNAT range. The probe runs only when `$PHUX_TAILSCALE` is unset.
//! Every failure degrades to detecting nothing.
//!
//! [`detect_magic_dns`] reads the same CLI's `status --json` for the host's
//! `MagicDNS` name, so a pairing link can name the host rather than an address
//! the tailnet may reassign.

use std::net::IpAddr;

/// Detect the host's overlay-network addresses, best effort (empty when
/// nothing is detected).
#[must_use]
pub fn detect() -> Vec<IpAddr> {
    detect_with_addresses(std::env::var_os("PHUX_OVERLAY_ADDRS").as_deref(), || {
        detect_from_override(std::env::var_os("PHUX_TAILSCALE").as_deref())
    })
}

/// Explicit addresses replace discovery, never augment it. In particular,
/// an invalid configuration must not expose a listener on a different network.
fn detect_with_addresses(
    addresses: Option<&std::ffi::OsStr>,
    discover: impl FnOnce() -> Vec<IpAddr>,
) -> Vec<IpAddr> {
    let Some(addresses) = addresses else {
        return discover();
    };
    addresses
        .to_str()
        .and_then(parse_explicit_addresses)
        .unwrap_or_else(|| {
            tracing::warn!(
                "invalid PHUX_OVERLAY_ADDRS; use comma-separated concrete unicast IPs \
             (no CIDRs, ports, or link-local addresses); overlay discovery is disabled"
            );
            Vec::new()
        })
}

/// Parse a comma-separated list atomically, preserving preference order.
/// Empty means disabled; one bad item refuses the whole list. These are
/// concrete unicast addresses, not interface names, CIDRs, or scoped IPv6.
fn parse_explicit_addresses(raw: &str) -> Option<Vec<IpAddr>> {
    let mut addresses = Vec::new();
    if raw.trim().is_empty() {
        return Some(addresses);
    }
    for item in raw.split(',') {
        let address: IpAddr = item.trim().parse().ok()?;
        if !is_explicit_unicast(address) {
            return None;
        }
        if !addresses.contains(&address) {
            addresses.push(address);
        }
    }
    Some(addresses)
}

/// Link-local IPv6 needs a scope the address-only detector cannot carry.
/// Reject mapped IPv4 too, so loopback/unspecified checks cannot be bypassed.
const fn is_explicit_unicast(address: IpAddr) -> bool {
    if address.is_loopback() || address.is_unspecified() || address.is_multicast() {
        return false;
    }
    match address {
        IpAddr::V4(address) => !address.is_broadcast() && !address.is_link_local(),
        IpAddr::V6(address) => {
            !address.is_unicast_link_local() && address.to_ipv4_mapped().is_none()
        }
    }
}

/// The host's Tailscale `MagicDNS` identity: its fully qualified name and the
/// tailnet addresses that name resolves to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MagicDns {
    /// The fully qualified `MagicDNS` name, without the trailing dot
    /// (`studio.tail9a8b.ts.net`).
    pub name: String,
    /// The node's tailnet addresses: the only addresses the name stands for.
    pub addresses: Vec<IpAddr>,
}

impl MagicDns {
    /// Whether the name resolves to `address` on the tailnet.
    #[must_use]
    pub fn names(&self, address: IpAddr) -> bool {
        self.addresses.contains(&address)
    }
}

/// Detect the host's `MagicDNS` name from `tailscale status --json`, best
/// effort.
///
/// `None` when `$PHUX_OVERLAY_ADDRS` is set (explicit addresses are
/// authoritative and may not be Tailscale's), when the CLI is missing, fails,
/// or is wedged, when the tailnet has `MagicDNS` disabled, or when the output
/// names no DNS name or address. `$PHUX_TAILSCALE` substitutes the CLI, as
/// for [`detect`].
#[must_use]
pub fn detect_magic_dns() -> Option<MagicDns> {
    if std::env::var_os("PHUX_OVERLAY_ADDRS").is_some() {
        return None;
    }
    let program = std::env::var_os("PHUX_TAILSCALE");
    let program = program
        .as_deref()
        .unwrap_or_else(|| std::ffi::OsStr::new("tailscale"));
    run_tailscale(program, &["status", "--json"], TAILSCALE_DEADLINE)
        .as_deref()
        .and_then(parse_tailscale_status)
}

/// Parse `tailscale status --json`: `Self.DNSName` (trailing dot trimmed)
/// and `Self.TailscaleIPs`, only when `CurrentTailnet.MagicDNSEnabled` is
/// true. A name the tailnet does not serve would pair a device with a host
/// it cannot resolve.
fn parse_tailscale_status(out: &str) -> Option<MagicDns> {
    let status: serde_json::Value = serde_json::from_str(out).ok()?;
    let enabled = status
        .pointer("/CurrentTailnet/MagicDNSEnabled")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    if !enabled {
        return None;
    }
    let me = status.get("Self")?;
    let name = me
        .get("DNSName")?
        .as_str()?
        .trim()
        .trim_end_matches('.')
        .to_ascii_lowercase();
    if !is_dns_name(&name) {
        return None;
    }
    let addresses: Vec<IpAddr> = me
        .get("TailscaleIPs")?
        .as_array()?
        .iter()
        .filter_map(|ip| ip.as_str()?.parse::<IpAddr>().ok())
        .filter(|ip| !ip.is_loopback() && !ip.is_unspecified())
        .collect();
    if addresses.is_empty() {
        return None;
    }
    Some(MagicDns { name, addresses })
}

/// A dotted DNS name a link can carry as a URL host unescaped: two or more
/// labels of letters, digits, and inner hyphens, at most 253 bytes.
fn is_dns_name(name: &str) -> bool {
    let label_ok = |label: &str| {
        (1..=63).contains(&label.len())
            && label
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
            && !label.starts_with('-')
            && !label.ends_with('-')
    };
    name.len() <= 253
        && name.contains('.')
        && name.parse::<IpAddr>().is_err()
        && name.split('.').all(label_ok)
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

/// How long a `tailscale` subcommand may run before it is killed: a wedged
/// tailscaled must never hang `phux pair`.
const TAILSCALE_DEADLINE: std::time::Duration = std::time::Duration::from_secs(2);

/// Run `<program> ip -4` and return its stdout, or `None` when it is missing,
/// fails, or outlives [`TAILSCALE_DEADLINE`] (it is then killed).
fn run_tailscale_ip(program: &std::ffi::OsStr) -> Option<String> {
    run_tailscale(program, &["ip", "-4"], TAILSCALE_DEADLINE)
}

/// Run `<program> <args>` and return its stdout, or `None` when it is
/// missing, fails, or outlives `deadline` (it is then killed).
fn run_tailscale(
    program: &std::ffi::OsStr,
    args: &[&str],
    deadline: std::time::Duration,
) -> Option<String> {
    let mut child = std::process::Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;

    // Drain stdout while the command runs: `status --json` lists every peer
    // and can outgrow a pipe buffer, which would stall the CLI until killed.
    let mut stdout = child.stdout.take()?;
    let (sender, output) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let read = std::io::Read::read_to_end(&mut stdout, &mut buf).map(|_| buf);
        let _ = sender.send(read);
    });

    let deadline = std::time::Instant::now() + deadline;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(25));
            }
            // Deadline expired: kill and reap so no zombie outlives us. The
            // reader ends when the last holder of the pipe exits; it is not
            // waited for.
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
    // A descendant that inherited the pipe could hold it open past the exit;
    // the deadline bounds the wait for end-of-file too.
    let remaining = deadline.saturating_duration_since(std::time::Instant::now());
    let buf = output.recv_timeout(remaining).ok()?.ok()?;
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
    fn explicit_addresses_replace_discovery_and_preserve_order() {
        let addresses = detect_with_addresses(
            Some(std::ffi::OsStr::new(" 10.77.0.2, fd77::2,10.77.0.2 ")),
            || panic!("explicit addresses must not consult tailscale or the route probe"),
        );
        assert_eq!(addresses, vec![ip("10.77.0.2"), ip("fd77::2")]);
        assert_eq!(
            detect_with_addresses(None, || vec![ip("100.99.98.97")]),
            vec![ip("100.99.98.97")],
        );
    }

    #[test]
    fn empty_or_invalid_explicit_addresses_never_fall_back() {
        for raw in [
            "",
            "  ",
            "bad",
            "10.77.0.2,bad",
            "10.77.0.2,",
            ",10.77.0.2",
            "10.77.0.2/24",
            "wg0",
            "10.77.0.2:8788",
            "[fd77::2]",
            "fe80::1%en0",
        ] {
            assert!(
                detect_with_addresses(Some(std::ffi::OsStr::new(raw)), || {
                    panic!("explicit {raw:?} must not fall back to another network")
                })
                .is_empty(),
                "invalid or empty explicit list {raw:?} must disable discovery",
            );
        }
    }

    #[test]
    fn explicit_addresses_reject_non_dialable_unicast() {
        for raw in [
            "0.0.0.0",
            "::",
            "127.0.0.1",
            "127.7.8.9",
            "::1",
            "224.0.0.1",
            "ff02::1",
            "255.255.255.255",
            "169.254.1.2",
            "fe80::1",
            "::ffff:127.0.0.1",
            "::ffff:0.0.0.0",
            "::ffff:10.77.0.2",
        ] {
            assert_eq!(parse_explicit_addresses(raw), None, "{raw}");
        }
        // Explicit selection is not a private-prefix policy: routed hosts
        // may use public addresses, but no public address is auto-discovered.
        assert_eq!(
            parse_explicit_addresses("192.0.2.2,2001:db8::2"),
            Some(vec![ip("192.0.2.2"), ip("2001:db8::2")]),
        );
    }

    #[cfg(unix)]
    #[test]
    fn non_unicode_explicit_addresses_do_not_trigger_discovery() {
        use std::os::unix::ffi::OsStrExt;

        assert!(
            detect_with_addresses(
                Some(std::ffi::OsStr::from_bytes(b"10.77.0.2,\xff")),
                || panic!("non-Unicode explicit addresses must not trigger discovery"),
            )
            .is_empty()
        );
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

    /// A trimmed `tailscale status --json`: the fields read, plus noise.
    fn status_json(enabled: &str, dns_name: &str, ips: &str) -> String {
        format!(
            r#"{{"Version":"1.90.0","BackendState":"Running",
            "Self":{{"HostName":"studio","DNSName":{dns_name},"TailscaleIPs":{ips},"Online":true}},
            "Peer":{{"nodekey:abc":{{"DNSName":"phone.tail9a8b.ts.net.","TailscaleIPs":["100.70.0.9"]}}}},
            "CurrentTailnet":{{"Name":"me@example.com","MagicDNSSuffix":"tail9a8b.ts.net","MagicDNSEnabled":{enabled}}}}}"#
        )
    }

    #[test]
    fn status_json_yields_the_magic_dns_name_and_its_addresses() {
        let out = status_json(
            "true",
            r#""studio.tail9a8b.ts.net.""#,
            r#"["100.101.102.103","fd7a:115c:a1e0::1"]"#,
        );
        let magic = parse_tailscale_status(&out).expect("MagicDNS");
        assert_eq!(magic.name, "studio.tail9a8b.ts.net", "trailing dot trimmed");
        assert_eq!(
            magic.addresses,
            vec![ip("100.101.102.103"), ip("fd7a:115c:a1e0::1")]
        );
        assert!(magic.names(ip("100.101.102.103")));
        assert!(
            !magic.names(ip("100.70.0.9")),
            "a peer's address is not ours"
        );
        assert!(!magic.names(ip("192.168.1.5")));
    }

    #[test]
    fn status_json_without_a_usable_magic_dns_name_yields_nothing() {
        let ips = r#"["100.101.102.103"]"#;
        let name = r#""studio.tail9a8b.ts.net.""#;
        for (why, out) in [
            ("MagicDNS disabled", status_json("false", name, ips)),
            ("MagicDNS flag missing", status_json("null", name, ips)),
            ("empty name", status_json("true", r#""""#, ips)),
            ("bare host", status_json("true", r#""studio.""#, ips)),
            ("unsafe name", status_json("true", r#""a b.ts.net.""#, ips)),
            ("no addresses", status_json("true", name, "[]")),
            ("not json", "100.101.102.103\n".to_owned()),
            ("empty", String::new()),
        ] {
            assert_eq!(parse_tailscale_status(&out), None, "{why}");
        }
    }

    /// End-to-end through the real spawn path, with output larger than a
    /// pipe buffer: a tailnet with many peers must not stall the CLI.
    #[cfg(unix)]
    #[test]
    fn stub_tailscale_status_round_trips_past_a_pipe_buffer() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().expect("tempdir");
        let doc = dir.path().join("status.json");
        let padding = "x".repeat(256 * 1024);
        let out = status_json(
            "true",
            r#""studio.tail9a8b.ts.net.""#,
            &format!(r#"["100.99.98.97"],"Padding":"{padding}""#),
        );
        std::fs::write(&doc, out).expect("write status");
        let script = dir.path().join("tailscale-stub");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\n[ \"$1 $2\" = \"status --json\" ] || exit 1\ncat '{}'\n",
                doc.display()
            ),
        )
        .expect("write stub");
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))
            .expect("chmod stub");

        let out = run_tailscale(
            script.as_os_str(),
            &["status", "--json"],
            std::time::Duration::from_secs(30),
        )
        .expect("stub output");
        let magic = parse_tailscale_status(&out).expect("MagicDNS");
        assert_eq!(magic.name, "studio.tail9a8b.ts.net");
        assert_eq!(magic.addresses, vec![ip("100.99.98.97")]);
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
        let out = run_tailscale(
            script.as_os_str(),
            &["ip", "-4"],
            std::time::Duration::from_secs(30),
        )
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
                run_tailscale(
                    script.as_os_str(),
                    &["ip", "-4"],
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
