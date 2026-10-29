//! Unit tests for overlay address and `MagicDNS` detection.

#![allow(clippy::expect_used, reason = "tests")]

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
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).expect("chmod stub");

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
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).expect("chmod stub");

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
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).expect("chmod stub");

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
