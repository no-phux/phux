//! QUIC pin/token parity and end-to-end relay credential privacy.

use super::*;

#[test]
fn direct_quic_cli_and_runtime_plan_equivalent_trust_facts() {
    let rt = crate::commands::build_cli_runtime().expect("runtime");
    let leaf = "ab".repeat(32);
    let ca = format!("sha256:{}", "cd".repeat(32));
    for case in [
        (
            "remote unpinned",
            "192.0.2.1:8788",
            None,
            None,
            None,
            Some("ab"),
            false,
        ),
        (
            "blank pins",
            "192.0.2.1:8788",
            Some(" "),
            Some(" "),
            None,
            Some("ab"),
            false,
        ),
        ("local dev", "127.0.0.1:8788", None, None, None, None, true),
        ("ipv6 dev", "[::1]:8788", None, None, None, None, true),
        (
            "leaf",
            "192.0.2.1:8788",
            Some(leaf.as_str()),
            None,
            None,
            Some(" ab "),
            true,
        ),
        (
            "CA only",
            "192.0.2.1:8788",
            None,
            Some(ca.as_str()),
            None,
            Some("ab"),
            true,
        ),
        (
            "CA wins",
            "192.0.2.1:8788",
            Some(leaf.as_str()),
            Some(ca.as_str()),
            None,
            None,
            true,
        ),
        (
            "bad token",
            "192.0.2.1:8788",
            Some(leaf.as_str()),
            None,
            None,
            Some("not-valid-secret"),
            false,
        ),
    ] {
        assert_quic_case(&rt, case);
    }
}

#[test]
fn relayed_quic_cli_and_runtime_keep_server_trust_and_identity_private() {
    let rt = crate::commands::build_cli_runtime().expect("runtime");
    let leaf = "ab".repeat(32);
    let ca = format!("sha256:{}", "cd".repeat(32));
    for case in [
        (
            "relay missing server CA",
            "192.0.2.1:8788",
            Some(leaf.as_str()),
            None,
            Some("route"),
            Some("ab"),
            false,
        ),
        (
            "relay cannot use server CA as relay pin",
            "192.0.2.1:8788",
            None,
            Some(ca.as_str()),
            Some("route"),
            Some("ab"),
            false,
        ),
        (
            "local relay still needs server CA",
            "127.0.0.1:8788",
            None,
            None,
            Some("route"),
            None,
            false,
        ),
        (
            "relay identity is private",
            "192.0.2.1:8788",
            Some(leaf.as_str()),
            Some(ca.as_str()),
            Some("route"),
            Some("ab"),
            true,
        ),
    ] {
        assert_quic_case(&rt, case);
    }
}

fn assert_quic_case(rt: &tokio::runtime::Runtime, case: NativeCase<'_>) {
    let (label, addr, pin, authority, route, token, allowed) = case;
    let credentials = parity_credentials(pin, authority, route, token);
    let mut target = phux_client_runtime::Target::quic(addr);
    target.cert_fingerprint = credentials.cert_fingerprint.clone();
    target.authority = credentials.authority.clone();
    target.tls_server_name = route.map(str::to_owned);
    target.token = credentials.token.clone();
    target.client_identity = credentials.identity.clone().expect("explicit identity");
    let cli = plan_quic_dial(rt, addr, credentials, target.tls_server_name.clone());
    let runtime = rt.block_on(target.plan());
    assert_eq!(cli.is_ok(), allowed, "CLI {label}");
    assert_eq!(runtime.is_ok(), allowed, "runtime {label}: {runtime:?}");
    if !allowed {
        return;
    }
    let Dial::Quic(cli) = cli.expect("allowed CLI plan").dial else {
        panic!("QUIC plan")
    };
    let phux_client_runtime::dial::Dial::Quic(runtime) = runtime.expect("allowed runtime plan")
    else {
        panic!("QUIC runtime plan")
    };
    assert_eq!(cli.addr, runtime.addr, "{label}");
    assert_eq!(cli.server_name, runtime.server_name, "{label}");
    assert_eq!(cli.token, runtime.token, "{label}");
    assert_eq!(cli.trust, runtime.trust, "{label}");
    assert_eq!(cli.identity, runtime.identity, "{label}");
    assert_eq!(cli.inner, runtime.inner, "{label}");
    if route.is_some() {
        assert_eq!(
            cli.trust,
            pin.map_or(CertTrust::SkipVerify, |pin| CertTrust::Pinned(pin.into()))
        );
        assert_eq!(cli.identity, Some(phux_dial::TlsClientIdentity::None));
        let inner = cli.inner.expect("end-to-end server session");
        assert_eq!(
            inner.trust,
            CertTrust::Authority {
                ca: authority.expect("server CA").into()
            }
        );
        assert_eq!(inner.identity, target.client_identity);
    } else if let Some(ca) = authority {
        assert_eq!(cli.trust, CertTrust::Authority { ca: ca.into() });
    }
}
