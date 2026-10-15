#![allow(clippy::expect_used, reason = "tests")]

use std::path::PathBuf;

use phux_config::{SatelliteConfigEntry, parse_str};

use crate::common;
use common::path;

/// Entries parse with `enabled` defaulting on, and serialize the token as a
/// path reference only, omitting absent auth keys.
#[test]
fn satellite_registry_entries_round_trip() {
    let input = r#"
[[satellites]]
name = "devbox"
endpoint = "ssh://devbox"

[[satellites]]
name = "lab"
endpoint = "quic://lab.example:8788"
enabled = false
token-file = "/secrets/lab-token"
cert-fingerprint = "AB:CD:EF:01"
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
                token_file: Some(PathBuf::from("/secrets/lab-token")),
                cert_fingerprint: Some("AB:CD:EF:01".to_owned()),
            },
        ]
    );

    let rendered = toml::to_string(&cfg).expect("serializes");
    assert!(rendered.contains(r#"token-file = "/secrets/lab-token""#));
    assert_eq!(rendered.matches("token-file").count(), 1, "{rendered}");
    assert_eq!(
        rendered.matches("cert-fingerprint").count(),
        1,
        "{rendered}"
    );
}
