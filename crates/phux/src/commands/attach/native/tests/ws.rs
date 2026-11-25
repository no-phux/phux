//! WebSocket TLS/token parity, development exceptions and relay refusal.

use super::*;

    #[test]
    fn websocket_remote_credential_refusals_match_runtime() {
        let rt = crate::commands::build_cli_runtime().expect("runtime");
        let leaf = "ab".repeat(32);
        for case in [
            (
                "plaintext remote",
                "ws://192.0.2.1:8787",
                Some(leaf.as_str()),
                None,
                None,
                Some("ab"),
                false,
            ),
            (
                "unpinned",
                "wss://192.0.2.1:8787",
                None,
                None,
                None,
                Some("ab"),
                false,
            ),
            (
                "blank pin",
                "wss://192.0.2.1:8787",
                Some(" "),
                None,
                None,
                Some("ab"),
                false,
            ),
            (
                "missing token",
                "wss://192.0.2.1:8787",
                Some(leaf.as_str()),
                None,
                None,
                None,
                false,
            ),
            (
                "empty token",
                "wss://192.0.2.1:8787",
                Some(leaf.as_str()),
                None,
                None,
                Some(" "),
                false,
            ),
            (
                "bad token",
                "wss://192.0.2.1:8787",
                Some(leaf.as_str()),
                None,
                None,
                Some("not-valid-secret"),
                false,
            ),
            (
                "odd hex token",
                "wss://192.0.2.1:8787",
                Some(leaf.as_str()),
                None,
                None,
                Some("a"),
                false,
            ),
        ] {
            assert_ws_case(&rt, case);
        }
    }

#[test]
fn websocket_pins_development_and_relay_refusal_match_runtime() {
    let rt = crate::commands::build_cli_runtime().expect("runtime");
    let leaf = "ab".repeat(32);
    let ca = format!("sha256:{}", "cd".repeat(32));
    for case in [
            ("dev", "ws://localhost:8787", None, None, None, None, true),
            ("ipv6 dev", "ws://[::1]:8787", None, None, None, None, true),
            (
                "leaf",
                "wss://192.0.2.1:8787",
                Some(leaf.as_str()),
                None,
                None,
                Some(" ab "),
                true,
            ),
            (
                "CA only",
                "wss://192.0.2.1:8787",
                None,
                Some(ca.as_str()),
                None,
                Some("ab"),
                true,
            ),
            (
                "CA wins",
                "wss://192.0.2.1:8787",
                Some(leaf.as_str()),
                Some(ca.as_str()),
                None,
                Some("ab"),
                true,
            ),
            (
                "relay cannot expose identity",
                "wss://192.0.2.1:8787",
                Some(leaf.as_str()),
                Some(ca.as_str()),
                Some("route"),
                Some("ab"),
                false,
            ),
            (
                "local relay cannot expose identity",
                "ws://localhost:8787",
                None,
                Some(ca.as_str()),
                Some("route"),
                None,
                false,
            ),
    ] {
        assert_ws_case(&rt, case);
    }
}

fn assert_ws_case(rt: &tokio::runtime::Runtime, case: NativeCase<'_>) {
    let (label, url, pin, authority, route, token, allowed) = case;
    let credentials = parity_credentials(pin, authority, route, token);
    let mut target = phux_client_runtime::Target::ws(url);
    target.cert_fingerprint = credentials.cert_fingerprint.clone();
    target.authority = credentials.authority.clone();
    target.token = credentials.token.clone();
    target.client_identity = credentials.identity.clone().expect("explicit identity");
    // A direct WSS SNI override is not itself an end-to-end relay route.
    target.tls_server_name = Some("server.example".into());
    let cli = plan_ws_dial(url.into(), credentials, target.tls_server_name.clone());
    let runtime = rt.block_on(target.plan());
    assert_eq!(cli.is_ok(), allowed, "CLI {label}");
    assert_eq!(runtime.is_ok(), allowed, "runtime {label}: {runtime:?}");
    if !allowed {
        return;
    }
    let Dial::Ws(cli) = cli.expect("allowed CLI plan").dial else {
        panic!("WebSocket plan")
    };
    let phux_client_runtime::dial::Dial::Ws(runtime) = runtime.expect("allowed runtime plan") else {
        panic!("WebSocket runtime plan")
    };
    assert_eq!(cli.url, runtime.url, "{label}");
    assert_eq!(cli.token, runtime.token, "{label}");
    assert_eq!(cli.trust, runtime.trust, "{label}");
    assert_eq!(cli.identity, runtime.identity, "{label}");
    assert_eq!(cli.tls_server_name, runtime.tls_server_name, "{label}");
    if let Some(ca) = authority {
        assert_eq!(cli.trust, CertTrust::Authority { ca: ca.into() });
    }
}
