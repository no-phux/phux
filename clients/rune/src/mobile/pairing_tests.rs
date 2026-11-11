//! Enrollment invite import and native trust-boundary coverage.

use super::{Configuration, parse_pairing_invite};
use phux_client_runtime::Transport;

fn phone_invite() -> String {
    format!(
        "https://phux.sh/connect?url=wss://mini.tailnet.ts.net:8787&quic=quic://mini.tailnet.ts.net:8788&name=Studio%20mini&fp={}&ca=sha256:{}&token={}&enroll={}",
        "ab".repeat(32),
        "cd".repeat(32),
        "ef".repeat(32),
        "12".repeat(32)
    )
}

#[test]
fn pairing_imports_current_phone_qr_without_merging_old_identity() {
    let imported = parse_pairing_invite(&phone_invite()).unwrap();
    assert_eq!(imported["name"], "Studio mini");
    assert_eq!(imported["ticket"], "12".repeat(32));
    assert_eq!(imported["peer"], "quic://mini.tailnet.ts.net:8788");
    let value = &imported["configuration"];
    assert_eq!(value["endpoint"], "quic://mini.tailnet.ts.net:8788");
    assert_eq!(value["certificate_pin"], "ab".repeat(32));
    assert_eq!(value["authority_ca"], format!("sha256:{}", "cd".repeat(32)));
    assert_eq!(value["token"], "ef".repeat(32));
    for absent in [
        "ticket",
        "enroll",
        "chain_pem",
        "key_id",
        "session",
        "tls_server_name",
    ] {
        assert!(value.get(absent).is_none());
    }
    let configuration: Configuration = serde_json::from_value(value.clone()).unwrap();
    assert!(configuration.enrollment_target().is_ok());
    assert!(configuration.validate_connection().is_err());
    for prefix in ["phux://connect", "phux-rune://connect"] {
        assert_eq!(
            parse_pairing_invite(&phone_invite().replace("https://phux.sh/connect", prefix))
                .unwrap(),
            imported
        );
    }
}

#[test]
fn pairing_imports_the_quic_only_link_emitted_by_native_tailnet_setup() {
    let direct = phone_invite().replace("url=wss://mini.tailnet.ts.net:8787&", "");
    assert_eq!(
        parse_pairing_invite(&direct).unwrap(),
        parse_pairing_invite(&phone_invite()).unwrap()
    );
    assert!(
        parse_pairing_invite(&direct.replace(&format!("&ca=sha256:{}", "cd".repeat(32)), ""))
            .is_err(),
        "QUIC-only imports still require explicit authority trust"
    );
    for (old, invalid) in [
        (
            "quic://mini.tailnet.ts.net:8788",
            "quic://mini.tailnet.ts.net:0",
        ),
        (
            "quic://mini.tailnet.ts.net:8788",
            "quic://user@mini.tailnet.ts.net:8788",
        ),
        (
            "quic://mini.tailnet.ts.net:8788",
            "ws://mini.tailnet.ts.net:8788",
        ),
    ] {
        assert!(parse_pairing_invite(&direct.replace(old, invalid)).is_err());
    }
}

#[test]
fn pairing_relay_sni_survives_configuration_and_authority_handshake() {
    let link = phone_invite().replace(
        "quic://mini.tailnet.ts.net:8788",
        "quic://relay.example:4433",
    ) + "&sni=mini-route";
    let imported = parse_pairing_invite(&link).unwrap();
    assert_eq!(imported["peer"], "mini-route via quic://relay.example:4433");
    assert_eq!(imported["configuration"]["tls_server_name"], "mini-route");
    let configuration: Configuration =
        serde_json::from_value(imported["configuration"].clone()).unwrap();
    let target = configuration.enrollment_target().unwrap();
    assert_eq!(target.tls_server_name.as_deref(), Some("mini-route"));
    assert_eq!(target.authority.route.as_deref(), Some("mini-route"));
    assert_eq!(
        target.transport,
        Transport::Quic("relay.example:4433".into())
    );
    let relay_only = link.replace("url=wss://mini.tailnet.ts.net:8787&", "");
    assert_eq!(parse_pairing_invite(&relay_only).unwrap(), imported);
}

#[test]
fn pairing_ignores_fallback_wss_metadata_and_never_imports_a_saved_chain() {
    let link = phone_invite().replace(
        "wss://mini.tailnet.ts.net:8787",
        "wss%3A%2F%2Fmini.tailnet.ts.net%3A8787%2Fsocket%3Fv%3D1",
    ) + "&chain_pem=old-chain&key_id=old-key&session=old-session";
    assert_eq!(
        parse_pairing_invite(&link).unwrap(),
        parse_pairing_invite(&phone_invite()).unwrap()
    );
    let no_label = phone_invite().replace("&name=Studio%20mini", "");
    assert!(parse_pairing_invite(&no_label).unwrap()["name"].is_null());
}

#[test]
fn pairing_requires_a_complete_enrollment_bundle_including_loopback() {
    for field in ["quic", "fp", "ca", "token", "enroll"] {
        let link = phone_invite();
        let (prefix, query) = link.split_once('?').unwrap();
        let query = query
            .split('&')
            .filter(|part| !part.starts_with(&format!("{field}=")))
            .collect::<Vec<_>>()
            .join("&");
        assert!(
            parse_pairing_invite(&format!("{prefix}?{query}")).is_err(),
            "{field}"
        );
    }
    for (field, old) in [
        ("token", "ef".repeat(32)),
        ("enroll", "12".repeat(32)),
        ("fp", "ab".repeat(32)),
    ] {
        assert!(
            parse_pairing_invite(&phone_invite().replace(&old, "short")).is_err(),
            "{field}"
        );
    }
    let loopback = phone_invite().replace("mini.tailnet.ts.net", "localhost");
    assert!(parse_pairing_invite(&loopback).is_ok());
    assert!(
        parse_pairing_invite(&loopback.replace(&format!("&fp={}", "ab".repeat(32)), "")).is_err()
    );
}

#[test]
fn pairing_rejects_malformed_input_without_echoing_secrets() {
    for suffix in [
        "\0",
        "\n",
        "\t",
        "#",
        "&token=other",
        "&t%6fken=other",
        "&name=duplicate",
        "&future=%00",
        "&future=%GG",
        "&future=%",
        "&future=%FF",
        "&future=%C2%85",
    ] {
        let link = phone_invite() + suffix;
        let error = parse_pairing_invite(&link).unwrap_err();
        assert!(!error.contains(&"ef".repeat(32)));
        assert!(!error.contains(&"12".repeat(32)));
        assert!(!error.contains(&link));
    }
    assert!(parse_pairing_invite(&(phone_invite() + &"a".repeat(16 * 1024))).is_err());
    assert!(parse_pairing_invite(&("\n".to_owned() + &phone_invite())).is_err());
    assert!(parse_pairing_invite("{\"endpoint\":\"quic://mini:8788\"}").is_err());
    for prefix in [
        "phux-rune://connect/",
        "phux-rune://connect.evil",
        "phux-rune://connect@evil",
        "phux-rune://Connect",
    ] {
        assert!(
            parse_pairing_invite(&phone_invite().replace("https://phux.sh/connect", prefix))
                .is_err()
        );
    }
}
