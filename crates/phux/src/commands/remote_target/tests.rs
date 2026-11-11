use super::{Bootstrap, RemoteTarget, endpoint_host, parse_target, with_port_override};
use crate::commands::remote::RemoteEntry;

fn entry(name: &str, endpoint: &str) -> RemoteEntry {
    RemoteEntry {
        name: name.to_owned(),
        endpoint: endpoint.to_owned(),
        ..RemoteEntry::default()
    }
}

/// A `wss://` entry's QUIC route is a kept `direct` one, else the same
/// host on the default QUIC port; nothing upgrades a QUIC, plaintext,
/// relay-routed, or bare-ssh entry.
#[test]
fn quic_candidate_derives_the_route_a_wss_entry_moves_to() {
    use super::quic_candidate;
    let candidate = |endpoint: &str| quic_candidate(&entry("mini", endpoint));
    assert_eq!(
        candidate("wss://mini.ts.net:8787").as_deref(),
        Some("quic://mini.ts.net:8788")
    );
    assert_eq!(
        candidate("wss://[fd7a::1]:8787").as_deref(),
        Some("quic://[fd7a::1]:8788"),
        "IPv6 stays bracketed"
    );
    assert_eq!(candidate("quic://mini:8788"), None);
    assert_eq!(candidate("ws://127.0.0.1:8787"), None);
    for unguessable in [
        "wss://127.0.0.1:8787",
        "wss://[::1]:8787",
        "wss://localhost:8787",
        "wss://mini:9787",
        "wss://mini",
    ] {
        assert_eq!(
            candidate(unguessable),
            None,
            "{unguessable}: nothing to guess"
        );
    }
    assert_eq!(candidate("ssh://mini"), None);

    let kept = RemoteEntry {
        direct: Some("quic://100.64.0.7:9000".to_owned()),
        ..entry("mini", "wss://100.64.0.7:8787")
    };
    assert_eq!(
        quic_candidate(&kept).as_deref(),
        Some("quic://100.64.0.7:9000"),
        "the paired QUIC route wins over the derived one"
    );
    let ssh = RemoteEntry {
        endpoint: "ssh://mini".to_owned(),
        ..kept.clone()
    };
    assert_eq!(
        quic_candidate(&ssh).as_deref(),
        Some("quic://100.64.0.7:9000")
    );
    let routed = RemoteEntry {
        tls_server_name: Some("mini-route".to_owned()),
        ..kept
    };
    assert_eq!(quic_candidate(&routed), None, "a relay has no ws leg");
}

/// An entry already on QUIC, or with nothing to try, is reported as such
/// without dialing anything.
#[test]
fn upgrade_to_quic_needs_no_dial_to_say_no() {
    use super::{QuicUpgrade, upgrade_to_quic};
    let deadline = std::time::Duration::from_millis(1);
    assert_eq!(
        upgrade_to_quic(&entry("mini", "quic://mini:8788"), deadline),
        QuicUpgrade::AlreadyQuic
    );
    assert_eq!(
        upgrade_to_quic(&entry("mini", "ssh://mini"), deadline),
        QuicUpgrade::NoRoute
    );
    assert_eq!(
        upgrade_to_quic(&entry("mini", "wss://mini:8787"), deadline),
        QuicUpgrade::NoRoute,
        "no token file: nothing to dial QUIC with"
    );
}

#[test]
fn endpoint_port_reads_direct_endpoints() {
    assert_eq!(super::endpoint_port("quic://mini:8788"), Some(8788));
    assert_eq!(super::endpoint_port("wss://[fd7a::1]:8787"), Some(8787));
    assert_eq!(super::endpoint_port("ssh://mini"), None);
}

#[test]
fn parse_labeled_words_errors_for_the_spelling_in_use() {
    let err = RemoteTarget::parse_labeled("", "host add").expect_err("empty");
    assert!(err.starts_with("host add needs a target"), "{err}");
    let err = RemoteTarget::parse_labeled("mini:0", "host add").expect_err("port 0");
    assert!(err.starts_with("host add port"), "{err}");
    assert!(
        parse_target("mini:0")
            .expect_err("port 0")
            .starts_with("--remote port")
    );
}

/// Every refusal names the CLI spelling, so an operator sees which flag
/// the grammar belongs to.
#[test]
fn every_refusal_names_the_remote_flag() {
    for (raw, prefix) in [
        ("", "--remote needs a target"),
        ("quic://mini:8788", "--remote takes [USER@]HOST[:PORT]"),
        ("@mini", "--remote target \"@mini\" has an empty user"),
        ("me@", "--remote target \"me@\" has an empty host"),
        ("../evil", "--remote host \"../evil\""),
        (
            "[fd7a::1",
            "--remote target \"[fd7a::1\" has an unclosed '['",
        ),
        (
            "[fd7a::1]x",
            "--remote target \"[fd7a::1]x\" has trailing text",
        ),
        ("mini:70000", "--remote port \"70000\""),
    ] {
        let err = parse_target(raw).expect_err(raw);
        assert!(err.starts_with(prefix), "{raw:?}: {err}");
    }
    let uri = parse_target("quic://mini:8788").expect_err("uri");
    assert!(uri.contains("phux host add"), "{uri}");
}

#[test]
fn endpoint_host_reads_every_registry_scheme() {
    assert_eq!(endpoint_host("quic://mini:8788").as_deref(), Some("mini"));
    assert_eq!(
        endpoint_host("wss://mini.ts.net:8787").as_deref(),
        Some("mini.ts.net")
    );
    assert_eq!(endpoint_host("ssh://mini").as_deref(), Some("mini"));
    assert_eq!(
        endpoint_host("quic://[fd7a::1]:8788").as_deref(),
        Some("fd7a::1")
    );
    assert_eq!(endpoint_host("not-a-uri"), None);
    assert_eq!(endpoint_host("quic://"), None);
}

#[test]
fn explicit_port_overrides_the_registered_endpoint() {
    let target = parse_target("mini:9999").expect("t");
    let overridden = with_port_override(entry("mini", "quic://mini:8788"), &target);
    assert_eq!(overridden.endpoint, "quic://mini:9999");

    // The scheme of a ws entry survives the override.
    let ws = with_port_override(entry("mini", "wss://mini:8787"), &target);
    assert_eq!(ws.endpoint, "wss://mini:9999");

    // ssh:// has no port in this grammar: leave it alone rather than
    // fabricating one the operator's ssh config may already answer.
    let ssh = with_port_override(entry("mini", "ssh://mini"), &target);
    assert_eq!(ssh.endpoint, "ssh://mini");
}

#[test]
fn no_explicit_port_leaves_the_entry_untouched() {
    let target = parse_target("mini").expect("t");
    let untouched = with_port_override(entry("mini", "quic://mini:9999"), &target);
    assert_eq!(
        untouched.endpoint, "quic://mini:9999",
        "a registered non-default port must survive a portless target"
    );
}

#[test]
fn bootstrap_never_is_distinct_from_auto() {
    assert_ne!(Bootstrap::Auto, Bootstrap::Never);
}
