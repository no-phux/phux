use super::*;
use crate::commands::doctor::Status;

/// Every `remote-cert` branch says something actionable, and doctor never
/// touches the certificate.
#[test]
fn remote_cert_reports_coverage_without_ever_repairing_it() {
    use phux_server::transport::tls::{cert_fingerprint, ensure_self_signed};

    let dir = tempfile::tempdir().expect("tempdir");
    let cert = dir.path().join("remote-cert.pem");
    let key = dir.path().join("remote-key.pem");
    let overlay = ["100.64.0.2".to_owned()];

    let check = remote_cert_check(&cert, &key, &overlay, false);
    assert_eq!(check.status, Status::Warn);
    assert!(check.hint.expect("hint").contains("phux pair"));

    ensure_self_signed(&cert, &key).expect("provision");
    let fingerprint = cert_fingerprint(&cert).expect("fingerprint");

    let check = remote_cert_check(&cert, &key, &overlay, false);
    assert_eq!(
        check.status,
        Status::Warn,
        "pinning consumers still work, so this is not a failed install"
    );
    assert!(check.detail.contains("100.64.0.2"));
    let hint = check.hint.expect("hint");
    assert!(hint.contains("un-pairing every paired device"), "{hint}");
    assert!(hint.contains(&cert.display().to_string()), "{hint}");
    assert!(hint.contains(&key.display().to_string()), "{hint}");

    let hint = remote_cert_check(&cert, &key, &overlay, true)
        .hint
        .expect("hint");
    assert!(hint.contains("subjectAltName"), "{hint}");
    assert!(!hint.contains("rm "), "{hint}");

    assert_eq!(
        remote_cert_check(&cert, &key, &[], false).status,
        Status::Warn
    );

    let wide_cert = dir.path().join("wide-cert.pem");
    let wide_key = dir.path().join("wide-key.pem");
    phux_server::transport::tls::ensure_self_signed_for(&wide_cert, &wide_key, &overlay)
        .expect("provision wide");
    let check = remote_cert_check(&wide_cert, &wide_key, &overlay, false);
    assert_eq!(check.status, Status::Pass);
    assert!(check.hint.is_none(), "a pass has nothing to remedy");

    assert_eq!(cert_fingerprint(&cert).expect("fingerprint"), fingerprint);
}

#[test]
fn remote_cert_chain_distinguishes_legacy_issued_and_unverifiable_material() {
    use phux_server::transport::tls::{cert_fingerprint, ensure_self_signed_for};
    use phux_server::workload::{WorkloadPaths, init_authority, issue_server_identity};

    let dir = tempfile::tempdir().expect("tempdir");
    let paths = WorkloadPaths {
        ca_cert: dir.path().join("ca.pem"),
        ca_key: dir.path().join("ca.key"),
        registry: dir.path().join("registry"),
    };
    let cert = dir.path().join("server.pem");
    let key = dir.path().join("server.key");
    let overlay = ["100.64.0.2".to_owned()];
    assert!(remote_cert_chain_check(&cert, &paths.ca_cert, false).is_none());
    init_authority(&paths.ca_cert, &paths.ca_key).expect("authority");
    ensure_self_signed_for(&cert, &key, &overlay).expect("legacy cert");
    let fingerprint = cert_fingerprint(&cert).expect("fingerprint");
    let legacy = remote_cert_chain_check(&cert, &paths.ca_cert, false).expect("legacy check");
    assert_eq!(legacy.status, Status::Warn);
    let hint = legacy.hint.as_deref().expect("hint");
    assert!(hint.contains("phone QR enrollment is blocked"), "{hint}");
    assert!(
        hint.contains("invalidates every paired client and workload credential"),
        "{hint}"
    );
    // A fully covered SAN does not conceal the chain problem.
    assert_eq!(
        remote_cert_check(&cert, &key, &overlay, false).status,
        Status::Pass
    );
    assert!(remote_cert_chain_check(&cert, &paths.ca_cert, true).is_none());
    assert_eq!(cert_fingerprint(&cert).expect("fingerprint"), fingerprint);

    let issued = issue_server_identity(&paths, &overlay).expect("issued identity");
    std::fs::write(&cert, issued.chain_pem()).expect("write fixture");
    let valid = remote_cert_chain_check(&cert, &paths.ca_cert, false).expect("valid check");
    assert_eq!(valid.status, Status::Pass);
    assert_eq!(
        remote_cert_check(&cert, &key, &overlay, false).status,
        Status::Pass
    );

    std::fs::write(&cert, "not a certificate").expect("malformed fixture");
    let malformed = remote_cert_chain_check(&cert, &paths.ca_cert, false).expect("malformed check");
    assert_eq!(malformed.status, Status::Warn);
    assert!(malformed.detail.contains("cannot verify"));
    assert!(!malformed.detail.contains("does not chain"));
    assert!(remote_cert_chain_check(&cert, &paths.ca_cert, true).is_none());
    assert_eq!(
        remote_cert_check(&cert, &key, &overlay, true).status,
        Status::Fail
    );
}
