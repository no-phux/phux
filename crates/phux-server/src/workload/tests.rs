//! Workload authority, registry, enrollment, and hot-reload tests.

use std::os::unix::fs::PermissionsExt;
use std::sync::{Arc, Barrier};

use super::*;

/// The CA key's file name: no diagnostic may print it.
const KEY_FILE: &str = "authority-secret-location.key";

struct Fixture {
    dir: tempfile::TempDir,
    paths: WorkloadPaths,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let paths = WorkloadPaths {
        ca_cert: dir.path().join("workload-ca.pem"),
        ca_key: dir.path().join(KEY_FILE),
        registry: dir.path().join("workload-keys"),
    };
    assert!(
        init_authority(&paths.ca_cert, &paths.ca_key)
            .unwrap()
            .created
    );
    Fixture { dir, paths }
}

fn in_an_hour() -> i64 {
    Utc::now().timestamp() + 3600
}

fn scopes(list: &[&str]) -> Vec<String> {
    list.iter().map(|scope| (*scope).to_owned()).collect()
}

/// A CSR for a fresh client key, as `phux workload add-key` receives it.
fn request() -> ClientMaterial {
    let key = KeyPair::generate().unwrap();
    let csr = CertificateParams::new(vec!["client".to_owned()])
        .unwrap()
        .serialize_request(&key)
        .unwrap();
    ClientMaterial::from_pem(csr.pem().unwrap().as_bytes()).unwrap()
}

fn leaf_of(chain: &str) -> CertificateDer<'static> {
    CertificateDer::pem_slice_iter(chain.as_bytes())
        .next()
        .unwrap()
        .unwrap()
}

fn self_signed() -> CertificateDer<'static> {
    let key = KeyPair::generate().unwrap();
    CertificateParams::new(vec!["client".to_owned()])
        .unwrap()
        .self_signed(&key)
        .unwrap()
        .der()
        .clone()
}

/// Enroll a fresh CSR: the issued leaf and the committed credential.
fn enroll(
    paths: &WorkloadPaths,
    scope: &[&str],
) -> (CertificateDer<'static>, RegisteredCredential) {
    let prepared = prepare_enrollment(paths, &request(), in_an_hour()).unwrap();
    let leaf = leaf_of(prepared.issued_chain_pem().unwrap());
    let registered = prepared
        .commit(&paths.registry, scopes(scope), in_an_hour())
        .unwrap();
    (leaf, registered)
}

/// Replace a file the way an editor would: a new inode at `mode`.
fn replace_with(path: &Path, bytes: &[u8], mode: u32) {
    let tmp = path.with_extension("edit");
    std::fs::write(&tmp, bytes).unwrap();
    std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(mode)).unwrap();
    std::fs::rename(&tmp, path).unwrap();
}

/// 16-character slices of a PEM key's base64 body.
fn key_needles(pem: &str) -> Vec<String> {
    let body: String = pem
        .lines()
        .filter(|line| !line.starts_with("-----"))
        .collect();
    body.as_bytes()
        .as_chunks::<16>()
        .0
        .iter()
        .map(|chunk| String::from_utf8(chunk.to_vec()).unwrap())
        .collect()
}

#[test]
fn ca_provisioning_is_stable_and_owner_only() {
    let fx = fixture();
    let first = ca_fingerprint(&fx.paths.ca_cert).unwrap();
    assert!(is_canonical_credential_id(&first), "{first}");
    let again = init_authority(&fx.paths.ca_cert, &fx.paths.ca_key).unwrap();
    assert_eq!(
        again,
        AuthorityStatus {
            fingerprint: first,
            created: false
        }
    );
    ensure_ca(&fx.paths.ca_cert, &fx.paths.ca_key).unwrap();
    for file in [&fx.paths.ca_cert, &fx.paths.ca_key] {
        let mode = std::fs::metadata(file).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
}

#[test]
fn a_partial_pair_is_refused_without_naming_either_path() {
    let fx = fixture();
    std::fs::remove_file(&fx.paths.ca_cert).unwrap();
    let error = init_authority(&fx.paths.ca_cert, &fx.paths.ca_key).unwrap_err();
    assert!(
        matches!(
            error,
            WorkloadError::PartialPair {
                present: "private key",
                ..
            }
        ),
        "{error}"
    );
    let rendered = format!("{error} {error:?}");
    assert!(
        !rendered.contains(&*fx.dir.path().to_string_lossy()),
        "{rendered}"
    );
}

#[test]
fn credential_ids_are_canonical() {
    let id = credential_id(b"public key");
    assert!(is_canonical_credential_id(&id));
    assert_eq!(id.len(), 71);
    let refused = [
        String::new(),
        "sha256:".to_owned(),
        format!("sha256:{}", id[7..].to_uppercase()),
        id[..70].to_owned(),
        format!("{id}0"),
        id.replace("sha256:", "sha512:"),
        format!(" {id}"),
    ];
    for candidate in refused {
        assert!(!is_canonical_credential_id(&candidate), "{candidate}");
    }
}

#[test]
fn a_signed_csr_is_a_client_certificate_the_handshake_verifier_accepts() {
    let fx = fixture();
    let prepared = prepare_enrollment(&fx.paths, &request(), in_an_hour()).unwrap();
    let chain = prepared.issued_chain_pem().unwrap();
    assert_eq!(chain.matches("BEGIN CERTIFICATE").count(), 2);
    let leaf = leaf_of(chain);
    crate::transport::tls::client_verifier(&authority_certificate(&fx.paths.ca_cert).unwrap())
        .unwrap()
        .verify_client_cert(&leaf, &[], UnixTime::now())
        .unwrap();

    let registered = prepared
        .commit(
            &fx.paths.registry,
            scopes(&["observe,input@terminal:3"]),
            in_an_hour(),
        )
        .unwrap();
    assert_eq!(registered.id, prepared.credential_id());
    assert_eq!(registered.generation, 1);
    let registry = WorkloadRegistry::load(&fx.paths.registry).unwrap();
    let credential = registry.lookup_certificate(leaf.as_ref()).unwrap();
    assert_eq!(credential.scopes, ["observe,input@terminal:3"]);
    assert_eq!(credential.authenticated(&registry).generation, 1);
}

#[test]
fn a_supplied_certificate_must_chain_to_this_authority() {
    let fx = fixture();
    let (leaf, _) = enroll(&fx.paths, &["observe@global"]);
    let issued = ClientMaterial::Certificate {
        leaf,
        intermediates: Vec::new(),
    };
    let prepared = prepare_enrollment(&fx.paths, &issued, in_an_hour()).unwrap();
    assert!(prepared.issued_chain_pem().is_none());
    assert!(matches!(
        prepared.commit(
            &fx.paths.registry,
            scopes(&["observe@global"]),
            in_an_hour()
        ),
        Err(WorkloadError::AlreadyRegistered(_))
    ));

    let other = fixture();
    let (foreign, _) = enroll(&other.paths, &["observe@global"]);
    for leaf in [foreign, self_signed()] {
        let material = ClientMaterial::Certificate {
            leaf,
            intermediates: Vec::new(),
        };
        assert!(matches!(
            prepare_enrollment(&fx.paths, &material, in_an_hour()),
            Err(WorkloadError::NotIssuedByAuthority)
        ));
    }
}

#[test]
fn enrollment_refuses_a_bad_request_scope_expiry_or_missing_authority() {
    let fx = fixture();
    let key = KeyPair::generate().unwrap();
    let mut der = CertificateParams::new(vec!["client".to_owned()])
        .unwrap()
        .serialize_request(&key)
        .unwrap()
        .der()
        .to_vec();
    let last = der.len() - 1;
    der[last] ^= 0x01;
    let tampered = ClientMaterial::Request(CertificateSigningRequestDer::from(der));
    assert!(matches!(
        prepare_enrollment(&fx.paths, &tampered, in_an_hour()),
        Err(WorkloadError::Material(MaterialError::InvalidRequest))
    ));

    let now = Utc::now().timestamp();
    for expiry in [now - 1, now, now + MAX_EXPIRY_SECONDS + 60] {
        assert!(matches!(
            prepare_enrollment(&fx.paths, &request(), expiry),
            Err(WorkloadError::InvalidExpiry)
        ));
    }

    let prepared = prepare_enrollment(&fx.paths, &request(), in_an_hour()).unwrap();
    assert!(matches!(
        prepared.commit(&fx.paths.registry, Vec::new(), in_an_hour()),
        Err(WorkloadError::NoScopes)
    ));
    assert!(matches!(
        prepared.commit(
            &fx.paths.registry,
            scopes(&["observe@global", "terminal.control"]),
            in_an_hour()
        ),
        Err(WorkloadError::InvalidScope { index: 2, .. })
    ));
    assert!(
        !fx.paths.registry.exists(),
        "a refused enrollment writes nothing"
    );

    let empty = tempfile::tempdir().unwrap();
    let missing = WorkloadPaths {
        ca_cert: empty.path().join("ca.pem"),
        ca_key: empty.path().join("ca.key"),
        registry: empty.path().join("keys"),
    };
    assert!(matches!(
        prepare_enrollment(&missing, &request(), in_an_hour()),
        Err(WorkloadError::AuthorityMissing)
    ));
}

#[test]
fn register_is_atomic_under_concurrent_writers() {
    const WRITERS: usize = 8;
    let fx = fixture();
    let registry = Arc::new(fx.paths.registry);
    let barrier = Arc::new(Barrier::new(WRITERS + 1));
    // Every writer is spawned before the barrier releases them together.
    let mut writers = Vec::with_capacity(WRITERS);
    for index in 1..=WRITERS {
        let (registry, barrier) = (Arc::clone(&registry), Arc::clone(&barrier));
        writers.push(std::thread::spawn(move || {
            barrier.wait();
            let key = [u8::try_from(index).unwrap(); 32];
            WorkloadRegistry::register(&registry, &key, scopes(&["observe@global"]), None).unwrap()
        }));
    }
    barrier.wait();
    // A reader racing the writers only ever sees whole generations.
    for _ in 0..200 {
        match WorkloadRegistry::load(&registry) {
            Ok(snapshot) => {
                assert_eq!(
                    snapshot.generation(),
                    u64::try_from(snapshot.len()).unwrap()
                );
            }
            Err(WorkloadError::Unstable) => {}
            Err(error) => panic!("a reader saw a torn registry: {error}"),
        }
    }
    let mut generations: Vec<u64> = writers
        .into_iter()
        .map(|writer| writer.join().unwrap().generation)
        .collect();
    generations.sort_unstable();
    assert_eq!(generations, (1..=8).collect::<Vec<u64>>(), "no lost update");
    let snapshot = WorkloadRegistry::load(&registry).unwrap();
    assert_eq!((snapshot.len(), snapshot.generation()), (WRITERS, 8));
}

#[test]
fn reload_observes_a_new_generation_and_a_malformed_file_yields_the_empty_snapshot() {
    let fx = fixture();
    let (first, registered) = enroll(&fx.paths, &["observe@global"]);
    let reloading = ReloadingWorkloadRegistry::load(fx.paths.registry.clone()).unwrap();
    assert_eq!(
        reloading
            .lookup_certificate(first.as_ref())
            .unwrap()
            .generation,
        1
    );

    let (second, _) = enroll(&fx.paths, &["input@terminal:1"]);
    assert_eq!(reloading.current().generation(), 2);
    assert_eq!(
        reloading
            .lookup_certificate(second.as_ref())
            .unwrap()
            .generation,
        2
    );
    let valid = std::fs::read(&fx.paths.registry).unwrap();

    replace_with(&fx.paths.registry, b"{ not json", 0o600);
    assert!(reloading.current().is_empty());
    assert!(
        reloading.lookup_certificate(first.as_ref()).is_none(),
        "a malformed generation never falls back to the last known-good one"
    );

    replace_with(&fx.paths.registry, &valid, 0o644);
    assert!(
        reloading.current().is_empty(),
        "an insecure registry admits no one"
    );

    replace_with(&fx.paths.registry, &valid, 0o600);
    assert_eq!(reloading.current().generation(), 2);
    assert!(reloading.lookup_certificate(first.as_ref()).is_some());

    WorkloadRegistry::revoke(&fx.paths.registry, &registered.id).unwrap();
    assert_eq!(reloading.current().generation(), 3);
    assert!(reloading.lookup_certificate(first.as_ref()).is_none());
    assert!(reloading.lookup_certificate(second.as_ref()).is_some());

    std::fs::remove_file(&fx.paths.registry).unwrap();
    assert!(reloading.current().is_empty());
}

#[test]
fn a_registry_that_breaks_a_rule_is_refused_at_startup() {
    let fx = fixture();
    enroll(&fx.paths, &["observe@global"]);
    let valid = std::fs::read_to_string(&fx.paths.registry).unwrap();
    let broken = [
        valid.replace("observe@global", "terminal.control"),
        valid.replace("\"generation\"", "\"surplus\""),
        valid.replace("\"version\": 1", "\"version\": 2"),
        "not json".to_owned(),
    ];
    for image in broken {
        replace_with(&fx.paths.registry, image.as_bytes(), 0o600);
        assert!(
            WorkloadRegistry::load(&fx.paths.registry).is_err(),
            "{image}"
        );
        assert!(ReloadingWorkloadRegistry::load(fx.paths.registry.clone()).is_err());
    }
    replace_with(&fx.paths.registry, valid.as_bytes(), 0o644);
    assert!(matches!(
        WorkloadRegistry::load(&fx.paths.registry),
        Err(WorkloadError::Insecure { .. })
    ));
}

#[test]
fn revoke_marks_the_record_and_is_idempotent() {
    let fx = fixture();
    let (leaf, registered) = enroll(&fx.paths, &["observe@global"]);
    let revoked = WorkloadRegistry::revoke(&fx.paths.registry, &registered.id).unwrap();
    assert!(revoked.newly_revoked);
    assert_eq!(revoked.generation, 2);
    let again = WorkloadRegistry::revoke(&fx.paths.registry, &registered.id).unwrap();
    assert_eq!(
        (again.newly_revoked, again.generation, again.revoked_at),
        (false, 2, revoked.revoked_at)
    );

    let registry = WorkloadRegistry::load(&fx.paths.registry).unwrap();
    assert!(registry.lookup_certificate(leaf.as_ref()).is_none());
    assert_eq!(
        registry.credentials()[0].revoked_at,
        Some(revoked.revoked_at)
    );

    // The record stays, so the same key cannot quietly come back.
    let reissue = ClientMaterial::Certificate {
        leaf,
        intermediates: Vec::new(),
    };
    let prepared = prepare_enrollment(&fx.paths, &reissue, in_an_hour()).unwrap();
    assert!(matches!(
        prepared.commit(
            &fx.paths.registry,
            scopes(&["observe@global"]),
            in_an_hour()
        ),
        Err(WorkloadError::AlreadyRegistered(_))
    ));

    assert!(matches!(
        WorkloadRegistry::revoke(&fx.paths.registry, &credential_id(b"nobody")),
        Err(WorkloadError::NotFound(_))
    ));
    let pasted = "-----BEGIN PRIVATE KEY-----MIIEvQIBADANBgkqhkiG9w0BAQEFAASC";
    let error = WorkloadRegistry::revoke(&fx.paths.registry, pasted).unwrap_err();
    assert!(matches!(error, WorkloadError::InvalidCredentialId));
    assert!(!format!("{error} {error:?}").contains("MIIEvQ"));
}

#[test]
fn debug_of_every_type_leaks_no_key_bytes() {
    let fx = fixture();
    let ca_key = std::fs::read_to_string(&fx.paths.ca_key).unwrap();
    let client_key = KeyPair::generate().unwrap().serialize_pem();
    let mut needles = key_needles(&ca_key);
    needles.extend(key_needles(&client_key));

    let material = request();
    let prepared = prepare_enrollment(&fx.paths, &material, in_an_hour()).unwrap();
    // A key's PEM also carries its algorithm identifiers and public point,
    // which certificates repeat; only slices no public artifact carries can
    // show a leak.
    let ca_certificate = std::fs::read_to_string(&fx.paths.ca_cert).unwrap();
    let public = [
        ca_certificate.as_str(),
        prepared.issued_chain_pem().unwrap(),
    ];
    needles.retain(|needle| public.iter().all(|text| !text.contains(needle.as_str())));
    assert!(needles.len() > 4, "enough secret-only slices to test");
    let registered = prepared
        .commit(&fx.paths.registry, scopes(&["*@global"]), in_an_hour())
        .unwrap();
    let revocation = WorkloadRegistry::revoke(&fx.paths.registry, &registered.id).unwrap();
    let registry = WorkloadRegistry::load(&fx.paths.registry).unwrap();
    let reloading = ReloadingWorkloadRegistry::load(fx.paths.registry.clone()).unwrap();
    let status = init_authority(&fx.paths.ca_cert, &fx.paths.ca_key).unwrap();

    let refused_key =
        WorkloadError::from(ClientMaterial::from_pem(client_key.as_bytes()).unwrap_err());
    std::fs::set_permissions(&fx.paths.ca_key, std::fs::Permissions::from_mode(0o644)).unwrap();
    let insecure_key = load_authority_key(&fx.paths.ca_key).unwrap_err();
    std::fs::remove_file(&fx.paths.ca_cert).unwrap();
    let partial = init_authority(&fx.paths.ca_cert, &fx.paths.ca_key).unwrap_err();

    let mut rendered = vec![
        format!("{:?}", fx.paths),
        format!("{material:?}"),
        format!("{prepared:?}"),
        format!("{registered:?}"),
        format!("{revocation:?}"),
        format!("{registry:?}"),
        format!("{reloading:?}"),
        format!("{:?}", registry.credentials()),
        format!("{status:?}"),
    ];
    for error in [
        refused_key,
        insecure_key,
        partial,
        WorkloadError::AuthorityMissing,
    ] {
        rendered.push(format!("{error} {error:?}"));
    }
    for text in &rendered {
        assert!(!text.contains(KEY_FILE), "the CA key path leaked: {text}");
        for needle in &needles {
            assert!(!text.contains(needle.as_str()), "key bytes leaked: {text}");
        }
    }
}

#[test]
fn the_registry_instance_is_minted_once_and_stamped_on_credentials() {
    let fx = fixture();
    let (_, registered) = enroll(&fx.paths, &["observe@global"]);
    let instance = WorkloadRegistry::load(&fx.paths.registry)
        .unwrap()
        .instance_id()
        .expect("the first write mints an instance")
        .to_owned();
    assert_eq!(instance.len(), 32);
    WorkloadRegistry::revoke(&fx.paths.registry, &registered.id).unwrap();
    let revoked = WorkloadRegistry::load(&fx.paths.registry).unwrap();
    assert_eq!(
        (revoked.instance_id(), revoked.generation()),
        (Some(instance.as_str()), 2),
        "later writes keep the instance and advance the generation"
    );

    let other = fixture();
    let (leaf, _) = enroll(&other.paths, &["observe@global"]);
    let reloading = ReloadingWorkloadRegistry::load(other.paths.registry.clone()).unwrap();
    let credential = reloading.lookup_certificate(leaf.as_ref()).unwrap();
    let other_instance = reloading.current().instance_id().unwrap().to_owned();
    assert_eq!(
        credential.registry_instance.as_deref(),
        Some(other_instance.as_str())
    );
    assert_ne!(
        other_instance, instance,
        "each registry gets its own instance"
    );

    let valid = std::fs::read_to_string(&fx.paths.registry).unwrap();
    replace_with(
        &fx.paths.registry,
        valid.replace(&instance, "not-hex").as_bytes(),
        0o600,
    );
    assert!(matches!(
        WorkloadRegistry::load(&fx.paths.registry),
        Err(WorkloadError::Malformed(_))
    ));
}

#[test]
fn a_transient_read_failure_is_not_cached() {
    let fx = fixture();
    enroll(&fx.paths, &["observe@global"]);
    let reloading = ReloadingWorkloadRegistry::load(fx.paths.registry.clone()).unwrap();
    let (second, _) = enroll(&fx.paths, &["input@terminal:1"]);
    store::fault::fail_next_read();
    assert!(
        reloading.current().is_empty(),
        "the lookup that failed admits no one"
    );
    assert_eq!(
        reloading.current().generation(),
        2,
        "the next lookup retries, with no change to the file"
    );
    assert!(reloading.lookup_certificate(second.as_ref()).is_some());
}

#[test]
fn enrollment_writes_a_client_chain_and_registry_record() {
    let fx = fixture();
    let client_cert = fx.dir.path().join("client.pem");
    let client_key = fx.dir.path().join("client.key");
    let id = enroll_client(
        &fx.paths.ca_cert,
        &fx.paths.ca_key,
        &client_cert,
        &client_key,
        &fx.paths.registry,
        scopes(&["observe@global"]),
    )
    .unwrap();
    assert!(is_canonical_credential_id(&id));
    assert_eq!(
        WorkloadRegistry::load(&fx.paths.registry)
            .unwrap()
            .lookup(&id)
            .unwrap()
            .scopes,
        ["observe@global"]
    );
    assert!(
        std::fs::read_to_string(&client_cert)
            .unwrap()
            .matches("BEGIN CERTIFICATE")
            .count()
            >= 2
    );
}
