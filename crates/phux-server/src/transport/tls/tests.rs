use super::*;
use std::fs;
use std::os::unix::fs::PermissionsExt;

fn fresh_pair() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let cert = dir.path().join("remote-cert.pem");
    let key = dir.path().join("remote-key.pem");
    ensure_self_signed(&cert, &key).unwrap();
    (dir, cert, key)
}

#[test]
fn ensure_self_signed_refuses_a_partial_pair() {
    let (_dir, cert, key) = fresh_pair();
    let fp = cert_fingerprint(&cert).unwrap();

    // Key lost, cert survives: refuse rather than rotate the pinned
    // fingerprint.
    fs::remove_file(&key).unwrap();
    let err = ensure_self_signed(&cert, &key).unwrap_err();
    assert!(matches!(err, TlsError::PartialTlsPair { .. }), "{err}");
    assert_eq!(cert_fingerprint(&cert).unwrap(), fp);

    fs::remove_file(&cert).unwrap();
    fs::write(&key, "not-a-real-key").unwrap();
    let err = ensure_self_signed(&cert, &key).unwrap_err();
    assert!(matches!(err, TlsError::PartialTlsPair { .. }), "{err}");
}

#[test]
fn ensure_self_signed_provisions_then_is_idempotent_and_builds() {
    let (dir, cert, key) = fresh_pair();
    let key_mode = fs::metadata(&key).unwrap().permissions().mode() & 0o777;
    assert_eq!(key_mode, 0o600, "private key must be owner-only");

    let fp1 = cert_fingerprint(&cert).unwrap();
    ensure_self_signed(&cert, &key).unwrap();
    assert_eq!(fp1, cert_fingerprint(&cert).unwrap());
    assert_eq!(fp1.matches(':').count(), 31);
    assert!(fp1.bytes().all(|b| b.is_ascii_hexdigit() || b == b':'));
    acceptor_from_pem(&cert, &key).unwrap();

    let missing = dir.path().join("nope.pem");
    assert!(acceptor_from_pem(&missing, &missing).is_err());
    assert!(cert_fingerprint(&missing).is_err());
}

/// An operator key (`PHUX_WS_TLS_KEY`) other accounts can read is refused
/// before any listener serves with it; a group-readable one (an
/// `ssl-cert` group setup) is used, with a warning.
#[test]
fn a_world_readable_key_is_refused_and_a_group_readable_one_served() {
    let (_dir, cert, key) = fresh_pair();
    fs::set_permissions(&key, fs::Permissions::from_mode(0o644)).unwrap();
    let Err(err) = acceptor_from_pem(&cert, &key) else {
        panic!("a world-readable key must be refused");
    };
    assert!(matches!(err, TlsError::InsecureKey(_)), "{err}");
    assert!(quic_server_config_with_client_ca(&cert, &key, None, false).is_err());

    fs::set_permissions(&key, fs::Permissions::from_mode(0o640)).unwrap();
    acceptor_from_pem(&cert, &key).unwrap();
    quic_server_config_with_client_ca(&cert, &key, None, false).unwrap();
}

#[test]
fn advertised_for_bind_names_only_a_dialable_address() {
    let advertised = |s: &str| advertised_for_bind(s.parse().unwrap());
    assert_eq!(advertised("100.64.0.2:8787"), vec!["100.64.0.2"]);
    assert_eq!(
        advertised("[fd7a:115c:a1e0::1]:8787"),
        vec!["fd7a:115c:a1e0::1"]
    );
    for bind in ["127.0.0.1:8787", "[::1]:8787", "0.0.0.0:8787", "[::]:8787"] {
        assert!(advertised(bind).is_empty(), "{bind}");
    }
}

/// ADR-0091: a wider request never reissues an existing certificate; the
/// gap is reported instead.
#[test]
fn an_existing_cert_is_never_widened() {
    let (_dir, cert, key) = fresh_pair();
    let fp = cert_fingerprint(&cert).unwrap();

    ensure_self_signed_for(&cert, &key, &["100.64.0.2".to_owned()]).unwrap();
    assert_eq!(cert_fingerprint(&cert).unwrap(), fp);
    assert!(!covers_name(&cert, "100.64.0.2").unwrap());
    assert_eq!(
        uncovered_names(&cert, &["100.64.0.2".to_owned(), "127.0.0.1".to_owned()]).unwrap(),
        vec!["100.64.0.2"],
    );
    // An unparseable name is uncovered, not a certificate error.
    assert!(!covers_name(&cert, "not a valid name").unwrap());
    assert!(!covers_name(&cert, "").unwrap());
}

#[test]
fn m_tls_acceptor_and_quic_config_accept_a_workload_ca() {
    let (dir, cert, key) = fresh_pair();
    let ca = dir.path().join("workload-ca.pem");
    let ca_key = dir.path().join("workload-ca.key");
    crate::workload::ensure_ca(&ca, &ca_key).unwrap();
    let ca = crate::workload::authority_certificate(&ca).unwrap();
    acceptor_from_pem_with_client_ca(&cert, &key, Some(&ca)).unwrap();
    quic_server_config_with_client_ca(&cert, &key, Some(&ca), true).unwrap();
}
