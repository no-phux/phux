//! `sync-managed`: one organization's records, and nothing else.

use chrono::Datelike as _;
use rcgen::PublicKeyData as _;

use super::*;

const ORG: &str = "no-phux";

fn sync(fx: &Fixture, org: &TestOrg, entries: &[serde_json::Value]) -> ManagedSync {
    WorkloadRegistry::sync_managed(&fx.paths.registry, org.name, &org.document(entries)).unwrap()
}

#[test]
fn a_sync_records_the_set_and_its_owner_only_roots_and_repeats_as_unchanged() {
    let fx = fixture();
    let org = TestOrg::new(ORG);
    let leaf = org.leaf(&KeyPair::generate().unwrap());

    let entry = managed_entry(&leaf, &["observe@global"]);
    let first = sync(&fx, &org, std::slice::from_ref(&entry));
    assert_eq!((first.added, first.removed, first.unchanged), (1, 0, 0));
    let registry = WorkloadRegistry::load(&fx.paths.registry).unwrap();
    let credential = registry
        .lookup_certificate(leaf_of(&leaf).as_ref())
        .unwrap();
    assert_eq!(credential.managed.as_deref(), Some(ORG));
    assert_eq!(credential.subject.as_deref(), Some("dev@example.com"));
    assert_eq!(credential.enrolled_for(), EnrolledFor::Human);
    let attested = credential.authenticated(&registry);
    assert_eq!(attested.managed.as_deref(), Some(ORG));

    let roots = org_roots_dir(&fx.paths.registry);
    let mode = |path: &Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&roots), 0o700);
    assert_eq!(mode(&roots.join("no-phux.pem")), 0o600);

    let again = sync(&fx, &org, &[entry]);
    assert_eq!((again.added, again.updated, again.removed), (0, 0, 0));
    assert_eq!(again.unchanged, 1);
    assert_eq!(
        again.generation, first.generation,
        "an unchanged sync writes nothing"
    );
}

#[test]
fn records_another_or_no_organization_manages_are_untouched() {
    let fx = fixture();
    let (_, host) = enroll(&fx.paths, &["observe@global"]);
    let other = TestOrg::new("other-org");
    let theirs = other.leaf(&KeyPair::generate().unwrap());
    sync(&fx, &other, &[managed_entry(&theirs, &["observe@global"])]);
    let before = WorkloadRegistry::load(&fx.paths.registry).unwrap();

    let org = TestOrg::new(ORG);
    let ours = org.leaf(&KeyPair::generate().unwrap());
    sync(&fx, &org, &[managed_entry(&ours, &["*@global"])]);
    let emptied = WorkloadRegistry::sync_managed(
        &fx.paths.registry,
        ORG,
        br#"{"v":1,"org":"no-phux","roots":[],"credentials":[]}"#,
    )
    .unwrap();
    assert_eq!(emptied.removed, 1);

    let after = WorkloadRegistry::load(&fx.paths.registry).unwrap();
    assert_eq!(after.credentials(), before.credentials());
    assert!(after.lookup(&host.id).is_some());
    assert!(
        !org_roots_dir(&fx.paths.registry)
            .join("no-phux.pem")
            .exists(),
        "an empty root set removes the organization's roots"
    );
}

#[test]
fn a_leaf_that_does_not_chain_to_the_roots_refuses_the_document() {
    let fx = fixture();
    let org = TestOrg::new(ORG);
    let foreign = TestOrg::new("elsewhere").leaf(&KeyPair::generate().unwrap());
    let ours = org.leaf(&KeyPair::generate().unwrap());
    let error = WorkloadRegistry::sync_managed(
        &fx.paths.registry,
        ORG,
        &org.document(&[
            managed_entry(&ours, &["*@global"]),
            managed_entry(&foreign, &["*@global"]),
        ]),
    )
    .unwrap_err();
    assert!(
        matches!(error, WorkloadError::NotIssuedByOrg { index: 2 }),
        "{error}"
    );
    assert!(!fx.paths.registry.exists(), "nothing was written");
    assert!(!org_roots_dir(&fx.paths.registry).exists());
}

#[test]
fn a_key_enrolled_outside_the_organization_is_never_taken_over() {
    let fx = fixture();
    let org = TestOrg::new(ORG);
    let key = KeyPair::generate().unwrap();
    WorkloadRegistry::register(
        &fx.paths.registry,
        &key.subject_public_key_info(),
        scopes(&["observe@global"]),
        None,
    )
    .unwrap();
    let error = WorkloadRegistry::sync_managed(
        &fx.paths.registry,
        ORG,
        &org.document(&[managed_entry(&org.leaf(&key), &["*@global"])]),
    )
    .unwrap_err();
    assert!(
        matches!(error, WorkloadError::AlreadyRegistered(_)),
        "{error}"
    );
}

#[test]
fn the_expiry_is_clamped_to_the_leaf_and_the_org_must_match() {
    let fx = fixture();
    let org = TestOrg::new(ORG);
    let mut entry = managed_entry(
        &org.leaf(&KeyPair::generate().unwrap()),
        &["observe@global"],
    );
    let leaf_expiry = Utc::now() + chrono::Duration::days(3);
    entry["expires_at"] = (Utc::now() + chrono::Duration::days(365))
        .to_rfc3339()
        .into();
    // The test leaf is valid until rcgen's default far-future date, so
    // clamp against a leaf with a near notAfter instead.
    let key = KeyPair::generate().unwrap();
    let mut params = CertificateParams::new(vec!["device".to_owned()]).unwrap();
    params.not_after = rcgen::date_time_ymd(
        leaf_expiry.year(),
        u8::try_from(leaf_expiry.month()).unwrap(),
        u8::try_from(leaf_expiry.day()).unwrap(),
    );
    params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ClientAuth];
    entry["certificate"] = org.sign(&params, &key).into();

    sync(&fx, &org, &[entry]);
    let registry = WorkloadRegistry::load(&fx.paths.registry).unwrap();
    let expires_at = registry.credentials()[0].expires_at.unwrap();
    assert!(expires_at <= leaf_expiry.timestamp(), "{expires_at}");

    let error = WorkloadRegistry::sync_managed(&fx.paths.registry, "other-org", &org.document(&[]))
        .unwrap_err();
    assert!(
        matches!(error, WorkloadError::ManagedDocument(_)),
        "{error}"
    );
}
