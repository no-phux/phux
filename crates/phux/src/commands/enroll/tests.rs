//! Unit tests for enrollment progress and the workload client identity.

use super::identity::{
    CertificateStatus, ClientIdentityFiles, HeldIdentity, RENEW_WITHIN_SECONDS, admission_ends,
    renewal_due,
};
use super::workload::{AddKeyReply, identity_stem};
use std::path::{Path, PathBuf};

#[test]
fn enrollment_progress_severity_follows_typed_outcomes() {
    use super::events::{EnrollEvent, Supervision};
    use crate::output::human_line;
    for (event, expected) in [
        (EnrollEvent::Paired, "phux: ok: paired"),
        (
            EnrollEvent::Probing {
                endpoint: "quic://mini:8788".to_owned(),
            },
            "phux: info: trying quic://mini:8788",
        ),
        (
            EnrollEvent::ServerStartFailed {
                error: "refused".to_owned(),
            },
            "phux: warn: could not start a server there (refused); pairing needs one with a remote listener, trying anyway",
        ),
        (
            EnrollEvent::ServerReady(Supervision::Unsupervised {
                reason: "no service manager".to_owned(),
            }),
            "phux: warn: server running unsupervised (service install failed: no service manager); it will not come back by itself after a reboot",
        ),
    ] {
        assert_eq!(
            human_line(
                event.severity(),
                format_args!("{}", event.describe()),
                false
            ),
            expected
        );
    }
}

/// A self-signed certificate valid until `days` from now (day
/// granularity), written as PEM to `path`.
pub(super) fn certificate_until(path: &Path, days: i64) {
    use chrono::Datelike as _;
    let key = rcgen::KeyPair::generate().expect("key");
    let mut params = rcgen::CertificateParams::new(vec!["client".to_owned()]).expect("params");
    let at = chrono::Utc::now() + chrono::Duration::days(days);
    let narrow = |value: u32| u8::try_from(value).expect("fits");
    params.not_after = rcgen::date_time_ymd(at.year(), narrow(at.month()), narrow(at.day()));
    let pem = params.self_signed(&key).expect("self-signed").pem();
    std::fs::write(path, pem).expect("write certificate");
}

/// A certificate is due for renewal when admission ends within the
/// window, has ended, or cannot be read; admission is taken to end the
/// day before `notAfter`, where issuance rounds the registry expiry.
#[test]
fn renewal_is_due_inside_the_window_expired_or_unreadable() {
    let dir = tempfile::tempdir().expect("tempdir");
    let now = chrono::Utc::now().timestamp();
    let path = |name: &str| dir.path().join(name);
    certificate_until(&path("long.pem"), 60);
    certificate_until(&path("edge.pem"), 16);
    certificate_until(&path("soon.pem"), 10);
    certificate_until(&path("gone.pem"), -2);
    std::fs::write(path("junk.pem"), "not a certificate").expect("junk");

    assert_eq!(renewal_due(&path("long.pem"), now), None);
    assert_eq!(
        renewal_due(&path("edge.pem"), now),
        None,
        "16 days is outside"
    );
    let ends = admission_ends(&path("long.pem")).expect("reads");
    assert!(
        ends - now > 58 * 86_400 && ends - now < 60 * 86_400,
        "{}",
        ends - now
    );
    let soon = renewal_due(&path("soon.pem"), now).expect("inside the window");
    assert!(soon.starts_with("expires on "), "{soon}");
    assert!(soon.contains("day(s)"), "{soon}");
    let gone = renewal_due(&path("gone.pem"), now).expect("expired");
    assert!(gone.starts_with("expired on "), "{gone}");
    for unreadable in ["junk.pem", "missing.pem"] {
        let why = renewal_due(&path(unreadable), now).expect("unreadable");
        assert!(why.contains("cannot be read"), "{why}");
    }
    assert!(
        renewal_due(&path("long.pem"), now + 60 * 86_400 - RENEW_WITHIN_SECONDS).is_some(),
        "the same certificate falls due as time passes"
    );
}

/// The `enrollment.status` vocabulary is stable, and only a failure
/// carries an error.
#[test]
fn certificate_status_strings_are_stable() {
    let failed = CertificateStatus::Failed("no phux".to_owned());
    assert_eq!(failed.as_str(), "failed");
    assert_eq!(failed.error(), Some("no phux"));
    for (status, text) in [
        (CertificateStatus::Enrolled, "enrolled"),
        (CertificateStatus::Kept, "kept"),
        (CertificateStatus::Skipped, "skipped"),
    ] {
        assert_eq!(status.as_str(), text);
        assert_eq!(status.error(), None);
    }
}

/// A half-named identity still names the credential it replaces when an
/// enrollment wrote it: from the certificate the entry names, or from
/// the enrolled certificate beside a lone enrolled key. Both files of
/// an enrolled pair are cleaned up; files the operator named are not.
#[test]
fn a_half_named_identity_is_retired_through_its_enrolled_pair() {
    let dir = tempfile::tempdir().expect("tempdir");
    let stem = identity_stem("mini", ID);
    let (cert, key) = (
        dir.path().join(format!("{stem}.pem")),
        dir.path().join(format!("{stem}.key")),
    );
    certificate_until(&cert, 30);
    let id = phux_server::workload::stored_credential_id(&cert).expect("id");
    let held = |certificate: Option<&PathBuf>, private_key: Option<&PathBuf>| HeldIdentity {
        certificate: certificate.cloned(),
        private_key: private_key.cloned(),
    };
    let both = vec![cert.clone(), key.clone()];

    let whole = held(Some(&cert), Some(&key));
    assert!(whole.pair().is_some());
    assert_eq!(
        whole.credential_id(dir.path(), "mini").as_deref(),
        Some(id.as_str())
    );
    assert_eq!(whole.enrolled_files(dir.path(), "mini"), both);

    let cert_only = held(Some(&cert), None);
    assert!(cert_only.pair().is_none() && !cert_only.is_empty());
    assert_eq!(
        cert_only.credential_id(dir.path(), "mini").as_deref(),
        Some(id.as_str())
    );
    assert_eq!(cert_only.enrolled_files(dir.path(), "mini"), both);

    let key_only = held(None, Some(&key));
    assert_eq!(
        key_only.credential_id(dir.path(), "mini").as_deref(),
        Some(id.as_str()),
        "the enrolled certificate beside the key names it"
    );
    assert_eq!(key_only.enrolled_files(dir.path(), "mini"), both);
    assert_eq!(
        key_only.credential_id(dir.path(), "other"),
        None,
        "not ours"
    );

    std::fs::remove_file(&cert).expect("lose the certificate");
    assert_eq!(key_only.credential_id(dir.path(), "mini"), None);

    let theirs = dir.path().join("mine.key");
    let operator = held(None, Some(&theirs));
    assert_eq!(operator.credential_id(dir.path(), "mini"), None);
    assert!(operator.enrolled_files(dir.path(), "mini").is_empty());
    assert!(held(None, None).is_empty());
}

pub(super) const ID: &str =
    "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

/// The far host's `add-key` reply is read past shell noise, and every
/// field it must carry is checked before the chain is even parsed.
#[test]
fn the_add_key_reply_is_untrusted_and_checked() {
    let doc = serde_json::json!({
        "schema_version": 1,
        "operation": "add-key",
        "credential_id": ID,
        "certificate_chain": "-----BEGIN CERTIFICATE-----",
        "replaced": true,
    });
    let pretty = serde_json::to_string_pretty(&doc).expect("json");
    let reply = AddKeyReply::parse(&format!("Welcome to box\n{pretty}\n")).expect("parse");
    assert_eq!(reply.credential_id, ID);

    let mut no_chain = doc.clone();
    no_chain
        .as_object_mut()
        .expect("object")
        .remove("certificate_chain");
    let older = AddKeyReply::parse(&no_chain.to_string()).expect_err("an older phux");
    assert!(older.contains("certificate_chain"), "{older}");

    let mut bad_id = doc.clone();
    bad_id["credential_id"] = serde_json::json!("sha256:ABC");
    assert!(AddKeyReply::parse(&bad_id.to_string()).is_err());

    let mut other = doc;
    other["operation"] = serde_json::json!("revoke");
    assert!(AddKeyReply::parse(&other.to_string()).is_err());
    assert!(AddKeyReply::parse("not json").is_err());
}

/// One pair of files per enrollment, named by the registry name and the
/// credential, so a re-enrollment never overwrites the pair in use.
#[test]
fn identity_files_are_named_per_credential() {
    assert_eq!(identity_stem("mini", ID), "mini.client.0123456789abcdef");
    assert_ne!(
        identity_stem("mini", ID),
        identity_stem("mini", &ID.replace("0123", "fedc"))
    );
}

/// Only a pair enrolled under this name in this directory is ever
/// cleaned up; files the operator named themselves are not.
#[test]
fn only_an_enrolled_pair_is_ours_to_remove() {
    let dir = Path::new("/state/remotes");
    let pair = |cert: &str, key: &str| ClientIdentityFiles {
        certificate: Path::new(cert).to_path_buf(),
        private_key: Path::new(key).to_path_buf(),
        credential_id: None,
        fresh: false,
    };
    let stem = identity_stem("mini", ID);
    let ours = pair(
        &format!("/state/remotes/{stem}.pem"),
        &format!("/state/remotes/{stem}.key"),
    );
    assert!(ours.enrolled_under(dir, "mini"));
    assert!(!ours.enrolled_under(dir, "mini2"), "another name's pair");
    assert!(!ours.enrolled_under(Path::new("/elsewhere"), "mini"));
    for (cert, key) in [
        ("/home/me/client.pem", "/home/me/client.key"),
        ("/state/remotes/mini.pem", "/state/remotes/mini.key"),
        (
            &*format!("/state/remotes/sub/{stem}.pem"),
            &*format!("/state/remotes/sub/{stem}.key"),
        ),
        (
            &*format!("/state/remotes/{stem}.pem"),
            "/home/me/client.key",
        ),
        (
            "/state/remotes/mini.client.not-hex.pem",
            "/state/remotes/mini.client.not-hex.key",
        ),
    ] {
        assert!(!pair(cert, key).enrolled_under(dir, "mini"), "{cert}");
    }
}
