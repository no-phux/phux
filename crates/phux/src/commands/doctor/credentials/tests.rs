use super::*;
use crate::commands::doctor::Status;

/// An unloadable (pre-versioned) store fails and names the remedy.
#[test]
fn an_unloadable_credential_store_fails_and_names_the_fix() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = dir.path().join("remote-tokens");

    assert_eq!(token_store_check(&store, None).status, Status::Pass);

    std::fs::write(&store, "deadbeef\n").expect("write legacy line");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&store, std::fs::Permissions::from_mode(0o600))
            .expect("the store rejects group/world-readable modes first");
    }
    let legacy = phux_server::auth::ReloadingTokenStore::load(store.clone())
        .expect_err("a pre-versioned store must refuse to load");

    let check = token_store_check(&store, Some(legacy));
    assert_eq!(
        check.status,
        Status::Fail,
        "a store that admits no device is a failure, not a warning"
    );
    assert!(
        check.detail.contains("remote listener"),
        "the detail must say what was lost, not just that a file is bad: {}",
        check.detail
    );
    let hint = check.hint.expect("a failure must carry a remedy");
    assert!(
        hint.contains("remove") && hint.contains("phux pair"),
        "the remedy for a pre-versioned store must say to remove it and pair again: {hint}"
    );
    assert!(
        hint.contains("restart") || hint.contains("upgrade"),
        "a server that predates lenient loading needs a restart to re-bind: {hint}"
    );
}

/// `workload-authority` fingerprints the CA and reports the registry
/// generation, never naming the CA private key.
#[test]
fn workload_authority_reports_fingerprint_and_generation() {
    let dir = tempfile::tempdir().expect("tempdir");
    let ca = dir.path().join("workload-ca.pem");
    let key = dir.path().join("workload-ca.key");
    let registry = dir.path().join("workload-keys");

    let missing = workload_authority_check(&ca, &registry);
    assert_eq!(missing.status, Status::Pass);
    assert!(
        missing.detail.contains("not initialized"),
        "{}",
        missing.detail
    );

    let status = phux_server::workload::init_authority(&ca, &key).expect("init");
    let fresh = workload_authority_check(&ca, &registry);
    assert_eq!(fresh.status, Status::Pass);
    assert!(
        fresh.detail.contains(&status.fingerprint),
        "{}",
        fresh.detail
    );
    assert!(fresh.detail.contains("generation 0"), "{}", fresh.detail);
    assert!(
        !fresh.detail.contains("workload-ca.key"),
        "{}",
        fresh.detail
    );

    // World-readable: refused, as the server would refuse it.
    std::fs::write(&registry, "{}").expect("write registry");
    let broken = workload_authority_check(&ca, &registry);
    assert_eq!(broken.status, Status::Fail);
    assert!(
        !broken.detail.contains("workload-ca.key"),
        "{}",
        broken.detail
    );
}

/// A self-signed certificate whose `notAfter` is `days` from now, as
/// PEM in `dir`; the doctor reads only its validity.
fn cert_expiring_in(dir: &std::path::Path, file: &str, days: i64) -> PathBuf {
    let key = rcgen::KeyPair::generate().unwrap();
    let mut params = rcgen::CertificateParams::new(vec!["client".to_owned()]).unwrap();
    let at = chrono::Utc::now() + chrono::Duration::days(days);
    params.not_after = rcgen::date_time_ymd(
        chrono::Datelike::year(&at),
        u8::try_from(chrono::Datelike::month(&at)).unwrap(),
        u8::try_from(chrono::Datelike::day(&at)).unwrap(),
    );
    let path = dir.join(file);
    std::fs::write(&path, params.self_signed(&key).unwrap().pem()).unwrap();
    path
}

fn remote(
    name: &str,
    cert: Option<PathBuf>,
    key: Option<&str>,
) -> crate::commands::remote::RemoteEntry {
    crate::commands::remote::RemoteEntry {
        index: 0,
        name: name.to_owned(),
        endpoint: format!("quic://{name}:8788"),
        token_file: None,
        cert_fingerprint: None,
        tls_server_name: None,
        session: None,
        ssh: Some(format!("me@{name}")),
        direct: None,
        client_cert: cert,
        client_key: key.map(PathBuf::from),
    }
}

/// Client certificates pass while good for more than the renewal
/// window, and one inside it, expired, unreadable, or half-named warns
/// with the command that fixes that remote.
#[test]
fn client_certificates_due_for_renewal_warn_with_the_remedy() {
    let dir = tempfile::tempdir().unwrap();
    let now = chrono::Utc::now().timestamp();
    let fresh = cert_expiring_in(dir.path(), "fresh.pem", 80);
    let soon = cert_expiring_in(dir.path(), "soon.pem", 5);
    let gone = cert_expiring_in(dir.path(), "gone.pem", -3);

    let none = client_certs_check(&[remote("bare", None, None)], now);
    assert_eq!(none.status, Status::Pass, "{none:?}");

    let good = client_certs_check(&[remote("mini", Some(fresh.clone()), Some("/k"))], now);
    assert_eq!(good.status, Status::Pass, "{good:?}");
    assert!(good.detail.contains("first expires"), "{good:?}");

    let due = client_certs_check(
        &[
            remote("mini", Some(fresh), Some("/k")),
            remote("soon", Some(soon), Some("/k")),
            remote("gone", Some(gone), Some("/k")),
            remote("lost", Some(dir.path().join("missing.pem")), Some("/k")),
            remote("half", None, Some("/k")),
        ],
        now,
    );
    assert_eq!(due.status, Status::Warn, "{due:?}");
    assert!(due.detail.contains("soon: expires on"), "{due:?}");
    assert!(due.detail.contains("gone: expired on"), "{due:?}");
    assert!(due.detail.contains("lost: at"), "{due:?}");
    assert!(due.detail.contains("half: names only one"), "{due:?}");
    assert!(!due.detail.contains("mini"), "{due:?}");
    let hint = due.hint.unwrap_or_default();
    for remedy in [
        "phux host renew soon",
        "phux host renew gone",
        "phux host renew lost",
        "phux host add me@half",
    ] {
        assert!(hint.contains(remedy), "{hint}");
    }
}
