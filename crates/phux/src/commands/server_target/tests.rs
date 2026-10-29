//! Unit tests for resolving the server a command targets.

use std::path::PathBuf;

use phux_client::attach::{AttachError, CertTrust, Dial};

use super::{
    ServerSpec, ServerTarget, dial_entry, entry_refusal, ladder_refusal_error, ssh_only_refusal,
};
use crate::commands::attach::DialRefusal;
use crate::commands::json_err::codes;
use crate::commands::remote::RemoteEntry;

fn runtime() -> tokio::runtime::Runtime {
    crate::commands::cli_runtime().expect("runtime")
}

fn entry(endpoint: &str, fingerprint: Option<&str>) -> RemoteEntry {
    RemoteEntry {
        index: 0,
        name: "mini".to_owned(),
        endpoint: endpoint.to_owned(),
        token_file: None,
        cert_fingerprint: fingerprint.map(str::to_owned),
        tls_server_name: None,
        session: None,
        ssh: None,
        direct: None,
        remote_phux: None,
        client_cert: None,
        client_key: None,
    }
}

/// No `--remote` means the local socket, exactly as before this module:
/// `--socket` when given, the default path otherwise.
#[test]
fn a_spec_without_remote_is_the_local_socket() {
    let rt = runtime();
    let explicit = ServerSpec::local(Some(PathBuf::from("/tmp/x.sock")))
        .resolve(&rt, "ls", false)
        .expect("local resolves");
    assert_eq!(
        explicit.socket_path(),
        Some(PathBuf::from("/tmp/x.sock").as_path())
    );
    assert!(!explicit.is_remote());

    let default = ServerSpec::default()
        .resolve(&rt, "ls", false)
        .expect("default resolves");
    assert_eq!(
        default.socket_path(),
        Some(phux_server::runtime::default_socket_path().as_path())
    );
}

/// `--socket` and `--remote` name two different servers; the helper
/// refuses the pair as a usage error rather than preferring one.
#[test]
fn socket_and_remote_are_mutually_exclusive() {
    let spec = ServerSpec {
        socket: Some(PathBuf::from("/tmp/x.sock")),
        remote: Some("mini".to_owned()),
    };
    let err = spec
        .resolve(&runtime(), "ls", false)
        .expect_err("must refuse");
    assert_eq!(err, std::process::ExitCode::from(2));
}

/// A malformed target is the same usage error attach reports, raised
/// before the registry or ssh are consulted.
#[test]
fn a_malformed_target_is_a_usage_error() {
    let spec = ServerSpec {
        socket: None,
        remote: Some("quic://mini:8788".to_owned()),
    };
    let err = spec
        .resolve(&runtime(), "ls", true)
        .expect_err("must refuse");
    assert_eq!(err, std::process::ExitCode::from(2));
}

/// A registered loopback QUIC entry dials without a pin, like
/// `phux attach --quic 127.0.0.1:PORT`.
#[test]
fn a_loopback_quic_entry_dials_without_a_pin() {
    let remote = dial_entry(
        &runtime(),
        &entry("quic://127.0.0.1:8788", None),
        "ls",
        false,
    )
    .expect("loopback quic dials");
    assert!(remote.loopback);
    let Dial::Quic(quic) = &remote.dial else {
        panic!("expected a QUIC dial, got {:?}", remote.dial);
    };
    assert!(matches!(quic.trust, CertTrust::SkipVerify));
    assert_eq!(quic.addr.port(), 8788);
}

/// A pinned routable entry carries its registered pin into the dial, on
/// both transports.
#[test]
fn a_pinned_routable_entry_carries_its_pin() {
    let quic = dial_entry(
        &runtime(),
        &entry("quic://203.0.113.7:8788", Some("ab")),
        "ls",
        false,
    )
    .expect("a pinned routable quic entry dials");
    assert!(!quic.loopback);
    let Dial::Quic(dial) = &quic.dial else {
        panic!("expected a QUIC dial, got {:?}", quic.dial);
    };
    // No CA is pinned for this leaf, so the dial pins the leaf and learns
    // nothing (ADR-0179).
    assert_eq!(dial.trust, CertTrust::Pinned("ab".to_owned()));

    // A routable wss:// dial also needs a bearer token, as under attach.
    let dir = tempfile::tempdir().expect("tempdir");
    let token_file = dir.path().join("mini.token");
    std::fs::write(&token_file, format!("{}\n", "a".repeat(64))).expect("write token");
    let mut wss_entry = entry("wss://203.0.113.7:8787", Some("cd"));
    wss_entry.token_file = Some(token_file);
    let wss = dial_entry(&runtime(), &wss_entry, "ls", false)
        .expect("a pinned, tokened routable wss entry dials");
    let Dial::Ws(dial) = &wss.dial else {
        panic!("expected a WebSocket dial, got {:?}", wss.dial);
    };
    assert_eq!(dial.trust, CertTrust::Pinned("cd".to_owned()));
}

/// Credential gaps are refused, as attach refuses them.
#[test]
fn unauthenticated_routable_entries_are_refused() {
    let rt = runtime();
    assert!(dial_entry(&rt, &entry("quic://203.0.113.7:8788", None), "ls", false).is_err());
    assert!(
        dial_entry(
            &rt,
            &entry("wss://203.0.113.7:8787", Some("ab")),
            "ls",
            false
        )
        .is_err(),
        "a routable wss dial with no token must refuse"
    );
    assert!(dial_entry(&rt, &entry("ssh://me@mini", None), "ls", true).is_err());
}

/// Every planner refusal is worded for the registry entry, never for
/// `phux attach`'s flags; only a failed name lookup is `transport`.
#[test]
fn planner_refusals_name_the_registry_entry() {
    let bad = entry("quic://203.0.113.7:8788", None);
    let refusals = [
        DialRefusal::Malformed("missing a port".to_owned()),
        DialRefusal::Unresolved {
            target: "ghost.invalid:8788".to_owned(),
            detail: "name resolution failed".to_owned(),
            dns_name: true,
        },
        DialRefusal::UnpinnedQuic {
            target: "203.0.113.7:8788".to_owned(),
        },
        DialRefusal::UnpinnedWs {
            url: "wss://203.0.113.7:8787".to_owned(),
        },
        DialRefusal::Plaintext {
            url: "ws://203.0.113.7:8787".to_owned(),
        },
        DialRefusal::NoToken {
            url: "wss://203.0.113.7:8787".to_owned(),
        },
        DialRefusal::BadToken("secret-shaped detail".to_owned()),
    ];
    for refusal in &refusals {
        let err = entry_refusal(&bad, refusal);
        let expected = if matches!(refusal, DialRefusal::Unresolved { .. }) {
            codes::TRANSPORT
        } else {
            codes::REMOTE_UNRESOLVED
        };
        assert_eq!(err.code, expected, "{refusal:?}");
        assert!(
            err.message.contains("\"mini\""),
            "{refusal:?}: {}",
            err.message
        );
        assert!(
            err.remedy.contains("phux host add mini"),
            "{refusal:?}: {}",
            err.remedy
        );
        let all = format!("{} {}", err.message, err.remedy);
        assert!(
            !all.contains("phux attach") && !all.contains("--quic") && !all.contains("--token "),
            "{refusal:?} leaks attach wording: {all}"
        );
        assert!(
            !all.contains("secret-shaped"),
            "a token detail must not be echoed"
        );
    }
}

/// The overlay hint belongs to a DNS name, never to an IP literal.
#[test]
fn only_a_dns_name_earns_the_overlay_hint() {
    let unresolved = |dns_name| DialRefusal::Unresolved {
        target: "x:1".to_owned(),
        detail: "no".to_owned(),
        dns_name,
    };
    let e = entry("quic://ghost.invalid:8788", None);
    assert!(
        entry_refusal(&e, &unresolved(true))
            .remedy
            .contains("overlay")
    );
    assert!(
        !entry_refusal(&e, &unresolved(false))
            .remedy
            .contains("overlay")
    );
}

/// The ssh:// refusal names the command actually being run.
#[test]
fn the_ssh_refusal_names_the_verb() {
    let err = ssh_only_refusal("mini", "me@mini", "kill");
    assert_eq!(err.code, codes::REMOTE_UNRESOLVED);
    assert!(
        err.remedy.contains("ssh me@mini phux kill"),
        "{}",
        err.remedy
    );
    assert!(!err.remedy.contains("phux ls"), "{}", err.remedy);
}

/// The ladder's multi-line refusal becomes one contract error: headline
/// as the message, remedies as the remedy, `phux:` prefixes gone.
#[test]
fn a_ladder_refusal_splits_into_message_and_remedy() {
    let err = ladder_refusal_error(
        "phux: mini is not a registered host\nphux: to pair without ssh, run `phux pair`\nphux:   phux --remote mini --code '<link>'",
    );
    assert_eq!(err.code, codes::REMOTE_UNRESOLVED);
    assert_eq!(err.message, "mini is not a registered host");
    assert_eq!(
        err.remedy,
        "to pair without ssh, run `phux pair`\nphux --remote mini --code '<link>'"
    );
}

/// A remote failure names the host and endpoint and maps onto the
/// shared transport vocabulary; the resolved target reports itself as
/// remote, with no local socket and the planned dial.
#[test]
fn a_remote_failure_names_the_host_and_uses_the_transport_codes() {
    let remote = dial_entry(
        &runtime(),
        &entry("quic://127.0.0.1:8788", None),
        "ls",
        false,
    )
    .expect("dials");
    let gone = remote.failure(&AttachError::Disconnected, "ls");
    assert_eq!(gone.code, codes::SERVER_DISCONNECTED);
    assert!(gone.message.contains("mini") && gone.message.contains("quic://127.0.0.1:8788"));

    let refused = remote.failure(&AttachError::Unreachable("timeout".to_owned()), "ls");
    assert_eq!(refused.code, codes::TRANSPORT);
    // Loopback never earns the overlay hint.
    assert!(
        refused.remedy.contains("phux host ls"),
        "{}",
        refused.remedy
    );

    let target = ServerTarget::Remote(Box::new(remote));
    assert!(target.is_remote());
    assert_eq!(target.socket_path(), None);
    assert!(matches!(target.dial(), Dial::Quic(_)));
}
