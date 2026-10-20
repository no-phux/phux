//! Link planning, SSH argv, token files, tails, and backoff.

use crate::hub::link::plan::host_is_loopback;

use super::*;

pub(super) fn entry(
    endpoint: &str,
    token_file: Option<&str>,
    cert_fingerprint: Option<&str>,
) -> HubEntry {
    HubEntry {
        target: crate::hub::parse_endpoint(endpoint).expect("valid endpoint"),
        token_file: token_file.map(PathBuf::from),
        cert_fingerprint: cert_fingerprint.map(str::to_owned),
    }
}

#[test]
fn quic_plan_keeps_host_port_and_pins_routable() {
    let spec = plan_link(&entry(
        "quic://devbox:8788",
        Some("/secrets/devbox.token"),
        Some("AB:CD"),
    ))
    .expect("dialable");
    assert_eq!(
        spec,
        DialSpec::Quic {
            host: "devbox".to_owned(),
            port: 8788,
            trust: CertTrust::Pinned("AB:CD".to_owned()),
            token_file: Some(PathBuf::from("/secrets/devbox.token")),
        }
    );
}

#[test]
fn wss_plan_keeps_url_and_pins_routable() {
    let spec = plan_link(&entry(
        "wss://sandbox:8787/phux",
        Some("/secrets/sandbox.token"),
        Some("ab:cd"),
    ))
    .expect("dialable");
    assert_eq!(
        spec,
        DialSpec::Ws {
            url: "wss://sandbox:8787/phux".to_owned(),
            trust: CertTrust::Pinned("ab:cd".to_owned()),
            token_file: Some(PathBuf::from("/secrets/sandbox.token")),
        }
    );
}

#[test]
fn loopback_plans_skip_verify_without_a_pin() {
    for endpoint in [
        "quic://127.0.0.1:8788",
        "quic://[::1]:8788",
        "quic://localhost:8788",
        "ws://127.0.0.1:8787",
        "wss://localhost:8787",
    ] {
        let spec = plan_link(&entry(endpoint, None, None)).expect("loopback carve-out");
        let trust = match spec {
            DialSpec::Quic { trust, .. } | DialSpec::Ws { trust, .. } => trust,
            DialSpec::Ssh { .. } | DialSpec::Inbound => {
                unreachable!("no ssh or inbound endpoints in this matrix")
            }
        };
        assert_eq!(trust, CertTrust::SkipVerify, "{endpoint}");
    }
}

#[test]
fn loopback_with_a_pin_still_pins() {
    let spec = plan_link(&entry("wss://127.0.0.1:8787", None, Some("AB"))).expect("dialable");
    assert!(matches!(spec, DialSpec::Ws { trust: CertTrust::Pinned(pin), .. } if pin == "AB"));
}

#[test]
fn routable_endpoints_fail_closed_without_token_or_pin() {
    // (endpoint, token, pin) -> expected refusal
    let matrix: &[(&str, Option<&str>, Option<&str>)] = &[
        ("quic://devbox:8788", None, None),
        ("quic://devbox:8788", Some("/t"), None),
        ("quic://devbox:8788", None, Some("AB")),
        ("wss://devbox:8787", None, None),
        ("wss://devbox:8787", Some("/t"), None),
        ("wss://devbox:8787", None, Some("AB")),
    ];
    for (endpoint, token, pin) in matrix {
        let refusal = plan_link(&entry(endpoint, *token, *pin)).expect_err("must fail closed");
        match (token, pin) {
            (_, None) => assert!(
                matches!(refusal, LinkRefusal::MissingFingerprint { .. }),
                "{endpoint} token={token:?} pin={pin:?}: {refusal:?}"
            ),
            (None, Some(_)) => assert!(
                matches!(refusal, LinkRefusal::MissingToken { .. }),
                "{endpoint} token={token:?} pin={pin:?}: {refusal:?}"
            ),
            (Some(_), Some(_)) => unreachable!("dialable rows are not in the matrix"),
        }
    }
}

#[test]
fn plaintext_ws_to_routable_host_is_refused_even_with_credentials() {
    let refusal = plan_link(&entry("ws://devbox:8787", Some("/t"), Some("AB")))
        .expect_err("plaintext routable");
    assert!(matches!(refusal, LinkRefusal::PlaintextRoutable { .. }));
}

#[test]
fn ssh_plan_needs_no_credentials_and_carries_none() {
    // Dialable with no auth material.
    let spec = plan_link(&entry("ssh://me@devbox:2222", None, None)).expect("dialable");
    assert_eq!(
        spec,
        DialSpec::Ssh {
            user: Some("me".to_owned()),
            host: "devbox".to_owned(),
            port: Some(2222),
        }
    );
    assert_eq!(spec.token_file(), None);

    // A configured token/pin on an ssh entry is ignored.
    let spec = plan_link(&entry("ssh://devbox", Some("/t"), Some("AB"))).expect("still dialable");
    assert_eq!(spec.token_file(), None, "{spec:?}");
}

#[test]
fn ssh_argv_matrix_is_shell_free_and_option_injection_proof() {
    assert_eq!(
        ssh_argv(None, "devbox", None),
        vec![
            "-o",
            "BatchMode=yes",
            "-o",
            "ClearAllForwardings=yes",
            "-o",
            "ServerAliveInterval=10",
            "-o",
            "ServerAliveCountMax=3",
            "-T",
            "--",
            "devbox",
            "phux",
            "stdio-bridge",
        ]
    );
    assert_eq!(
        ssh_argv(Some("me"), "devbox", Some(2222)),
        vec![
            "-o",
            "BatchMode=yes",
            "-o",
            "ClearAllForwardings=yes",
            "-o",
            "ServerAliveInterval=10",
            "-o",
            "ServerAliveCountMax=3",
            "-T",
            "-p",
            "2222",
            "-l",
            "me",
            "--",
            "devbox",
            "phux",
            "stdio-bridge",
        ]
    );
    // interval * count == LINK_IDLE_TIMEOUT.
    assert_eq!(
        LINK_KEEPALIVE_INTERVAL * 3,
        LINK_IDLE_TIMEOUT,
        "keepalive constants drifted; re-derive ServerAlive* expectations"
    );
    // The host always follows `--`.
    let argv = ssh_argv(Some("me"), "devbox", Some(22));
    let dashdash = argv.iter().position(|a| a == "--").expect("has --");
    assert_eq!(argv[dashdash + 1], "devbox");
}

#[test]
fn quic_loopback_literals_are_recognized() {
    assert!(host_is_loopback("127.0.0.1"));
    assert!(host_is_loopback("[::1]"));
    assert!(host_is_loopback("LOCALHOST"));
    assert!(!host_is_loopback("devbox"));
    assert!(!host_is_loopback("10.0.0.7"));
    assert!(!host_is_loopback("[2001:db8::1]"));
}

#[test]
fn token_file_first_nonempty_line_wins_and_hex_is_enforced() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("satellite.token");

    std::fs::write(&path, "deadbeef\n").expect("write");
    assert_eq!(
        read_link_token(Some(&path)).expect("valid"),
        Some("deadbeef".to_owned())
    );

    std::fs::write(&path, "\n  \ndeadbeef\nc0ffee\n").expect("write");
    assert_eq!(
        read_link_token(Some(&path)).expect("line-oriented"),
        Some("deadbeef".to_owned())
    );

    std::fs::write(&path, "not hex!\n").expect("write");
    let err = read_link_token(Some(&path)).expect_err("not hex");
    assert!(err.contains("valid hex"), "{err}");

    std::fs::write(&path, "\n\n").expect("write");
    let err = read_link_token(Some(&path)).expect_err("empty");
    assert!(err.contains("empty"), "{err}");

    let missing = dir.path().join("nope.token");
    let err = read_link_token(Some(&missing)).expect_err("missing");
    assert!(err.contains("read token file"), "{err}");

    assert_eq!(read_link_token(None).expect("no file configured"), None);
}

#[tokio::test]
async fn read_tail_is_bounded_and_keeps_the_end() {
    // Only the final `max` bytes survive.
    let mut input = vec![b'x'; 1024 * 1024];
    input.extend_from_slice(b"END-MARKER");
    let mut reader = input.as_slice();
    let tail = read_tail(&mut reader, SSH_STDERR_TAIL_MAX).await;
    assert_eq!(tail.len(), SSH_STDERR_TAIL_MAX);
    assert!(tail.ends_with(b"END-MARKER"));

    // Short input passes through untouched.
    let mut reader = &b"auth refused\n"[..];
    assert_eq!(
        read_tail(&mut reader, SSH_STDERR_TAIL_MAX).await,
        b"auth refused\n"
    );
}

#[test]
fn backoff_doubles_from_base_and_caps() {
    let mut backoff = Backoff::new(Duration::from_millis(500), Duration::from_secs(30));
    let mut delays = Vec::new();
    for _ in 0..9 {
        delays.push(backoff.next_delay());
    }
    assert_eq!(
        delays,
        vec![
            Duration::from_millis(500),
            Duration::from_secs(1),
            Duration::from_secs(2),
            Duration::from_secs(4),
            Duration::from_secs(8),
            Duration::from_secs(16),
            Duration::from_secs(30),
            Duration::from_secs(30),
            Duration::from_secs(30),
        ]
    );
}

#[test]
fn backoff_reset_returns_to_base() {
    let mut backoff = Backoff::new(Duration::from_millis(500), Duration::from_secs(30));
    let _ = backoff.next_delay();
    let _ = backoff.next_delay();
    backoff.reset();
    assert_eq!(backoff.failures(), 0);
    assert_eq!(backoff.next_delay(), Duration::from_millis(500));
}

#[test]
fn backoff_never_overflows_at_large_failure_counts() {
    let mut backoff = Backoff::new(Duration::from_millis(500), Duration::from_secs(30));
    for _ in 0..10_000 {
        assert!(backoff.next_delay() <= Duration::from_secs(30));
    }
}
