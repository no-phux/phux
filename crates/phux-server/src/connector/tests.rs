use super::*;
use std::io::Write as _;

fn entry(
    relay: &str,
    token_file: Option<&str>,
    fingerprint: Option<&str>,
) -> phux_config::ConnectorConfigEntry {
    phux_config::ConnectorConfigEntry {
        relay: relay.to_owned(),
        token_file: token_file.map(PathBuf::from),
        cert_fingerprint: fingerprint.map(str::to_owned),
    }
}

#[test]
fn routable_relays_require_pin_and_token() {
    assert!(matches!(
        plan_connector(&entry("relay.example:4433", Some("/token"), None)),
        Err(ConnectorError::MissingFingerprint { .. })
    ));
    assert!(matches!(
        plan_connector(&entry("relay.example:4433", None, Some("AB"))),
        Err(ConnectorError::MissingToken { .. })
    ));
}

#[test]
fn loopback_keeps_the_dev_trust_carveout() {
    let plan = plan_connector(&entry("[::1]:4433", None, None)).expect("loopback plan");
    assert!(matches!(plan.trust, CertTrust::SkipVerify));
    assert_eq!(plan.host, "::1");
    assert_eq!(plan.port, 4433);
}

#[test]
fn malformed_relay_is_rejected_before_dial() {
    for relay in ["localhost", ":4433", "localhost:0", "[::1:4433"] {
        assert!(matches!(
            plan_connector(&entry(relay, None, None)),
            Err(ConnectorError::Malformed { .. })
        ));
    }
}

#[test]
fn token_file_must_be_owner_only_and_valid_hex() {
    let mut file = tempfile::NamedTempFile::new().expect("token file");
    writeln!(file, "{}", "11".repeat(32)).expect("write token");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(file.path(), std::fs::Permissions::from_mode(0o644))
            .expect("set broad mode");
        let error = read_connector_token(Some(file.path())).expect_err("broad mode refused");
        assert!(error.contains("chmod 600"), "{error}");
        std::fs::set_permissions(file.path(), std::fs::Permissions::from_mode(0o600))
            .expect("set owner-only mode");
    }
    assert_eq!(
        read_connector_token(Some(file.path())).expect("valid token"),
        Some(vec![0x11; 32])
    );
}

#[test]
fn token_file_is_reread_and_length_checked() {
    let file = tempfile::NamedTempFile::new().expect("token file");
    std::fs::write(
        file.path(),
        format!("{}\n", "11".repeat(crate::auth::TOKEN_LEN)),
    )
    .expect("write first token");
    assert_eq!(
        read_connector_token(Some(file.path())).expect("first token"),
        Some(vec![0x11; crate::auth::TOKEN_LEN])
    );

    std::fs::write(
        file.path(),
        format!("{}\n", "22".repeat(crate::auth::TOKEN_LEN)),
    )
    .expect("rotate token");
    assert_eq!(
        read_connector_token(Some(file.path())).expect("rotated token"),
        Some(vec![0x22; crate::auth::TOKEN_LEN])
    );

    std::fs::write(file.path(), "22\n").expect("write short token");
    let error = read_connector_token(Some(file.path())).expect_err("short token refused");
    assert!(error.contains("exactly 32 token bytes"), "{error}");
}
