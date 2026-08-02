#![allow(clippy::expect_used, reason = "tests")]

use std::path::PathBuf;

use phux_config::{SatelliteConfigEntry, parse_str};

mod common;
use common::path;

#[test]
fn satellite_registry_entries_parse() {
    let input = r#"
[[satellites]]
name = "devbox"
endpoint = "ssh://devbox"

[[satellites]]
name = "lab"
endpoint = "quic://lab.example:8788"
enabled = false
"#;
    let cfg = parse_str(input, &path()).expect("satellite registry parses");

    assert_eq!(
        cfg.satellites,
        vec![
            SatelliteConfigEntry {
                name: "devbox".to_owned(),
                endpoint: "ssh://devbox".to_owned(),
                enabled: true,
                token_file: None,
                cert_fingerprint: None,
            },
            SatelliteConfigEntry {
                name: "lab".to_owned(),
                endpoint: "quic://lab.example:8788".to_owned(),
                enabled: false,
                token_file: None,
                cert_fingerprint: None,
            },
        ]
    );
}

#[test]
fn satellite_auth_material_parses() {
    let input = r#"
[[satellites]]
name = "lab"
endpoint = "quic://lab.example:8788"
token-file = "/home/hub/.local/state/phux/satellite-tokens/lab"
cert-fingerprint = "AB:CD:EF:01"
"#;
    let cfg = parse_str(input, &path()).expect("satellite auth material parses");

    assert_eq!(
        cfg.satellites,
        vec![SatelliteConfigEntry {
            name: "lab".to_owned(),
            endpoint: "quic://lab.example:8788".to_owned(),
            enabled: true,
            token_file: Some(PathBuf::from(
                "/home/hub/.local/state/phux/satellite-tokens/lab"
            )),
            cert_fingerprint: Some("AB:CD:EF:01".to_owned()),
        }]
    );
}

#[test]
fn satellite_auth_material_roundtrips_without_the_secret() {
    let input = r#"
[[satellites]]
name = "lab"
endpoint = "quic://lab.example:8788"
token-file = "/secrets/lab-token"
cert-fingerprint = "abcd"
"#;
    let cfg = parse_str(input, &path()).expect("parses");
    let rendered = toml::to_string(&cfg).expect("serializes");

    // The registry stores a *path* to the token, never token bytes; the
    // serialized form carries the same reference and nothing more.
    assert!(rendered.contains(r#"token-file = "/secrets/lab-token""#));
    assert!(rendered.contains(r#"cert-fingerprint = "abcd""#));
}

#[test]
fn satellite_entry_without_auth_serializes_without_auth_keys() {
    let input = r#"
[[satellites]]
name = "devbox"
endpoint = "ssh://devbox"
"#;
    let cfg = parse_str(input, &path()).expect("parses");
    let rendered = toml::to_string(&cfg).expect("serializes");

    assert!(
        !rendered.contains("token-file") && !rendered.contains("cert-fingerprint"),
        "absent auth material must not serialize as explicit keys: {rendered}"
    );
}
