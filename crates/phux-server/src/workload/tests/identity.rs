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
    let (second, _) = enroll(&fx.paths, &["input@host:devbox"]);
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

/// An issued leaf's subject common names, URI SANs, and DNS SANs.
fn leaf_names(leaf: &CertificateDer<'_>) -> (Vec<String>, Vec<String>, Vec<String>) {
    let (_, parsed) = x509_parser::parse_x509_certificate(leaf.as_ref()).unwrap();
    let common_names = parsed
        .subject()
        .iter_common_name()
        .map(|cn| cn.as_str().unwrap().to_owned())
        .collect();
    let (mut uris, mut dns) = (Vec::new(), Vec::new());
    if let Ok(Some(san)) = parsed.subject_alternative_name() {
        for name in &san.value.general_names {
            match name {
                x509_parser::extensions::GeneralName::URI(uri) => uris.push((*uri).to_owned()),
                x509_parser::extensions::GeneralName::DNSName(host) => {
                    dns.push((*host).to_owned());
                }
                _ => {}
            }
        }
    }
    (common_names, uris, dns)
}

/// ADR-0116: an issued certificate names its credential id (CN and URI SAN),
/// not the constant `phux-workload-client`.
#[test]
fn an_issued_certificate_names_its_credential_id() {
    let fx = fixture();
    let (leaf, registered) = enroll(&fx.paths, &["observe@global"]);
    let (common_names, uris, dns) = leaf_names(&leaf);
    assert_eq!(
        common_names.as_slice(),
        std::slice::from_ref(&registered.id)
    );
    assert_eq!(uris.as_slice(), std::slice::from_ref(&registered.id));
    assert!(dns.is_empty(), "no DNS name: {dns:?}");
    let (_, parsed) = x509_parser::parse_x509_certificate(leaf.as_ref()).unwrap();
    assert!(!parsed.is_ca());
}

/// A certificate issued before the subject named the credential id (SAN
/// `phux-workload-client`) admits exactly as before: admission derives the
/// id from the verified key and never reads the subject.
#[test]
fn a_certificate_issued_under_the_old_subject_still_admits() {
    let fx = fixture();
    let key = KeyPair::generate().unwrap();
    let mut params = CertificateParams::new(vec!["phux-workload-client".to_owned()]).unwrap();
    params
        .distinguished_name
        .push(DnType::CommonName, "phux workload client");
    params.is_ca = IsCa::ExplicitNoCa;
    params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
    params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ClientAuth];
    let issuer = Issuer::from_ca_cert_der(
        &authority_certificate(&fx.paths.ca_cert).unwrap(),
        load_authority_key(&fx.paths.ca_key).unwrap(),
    )
    .unwrap();
    let leaf = params.signed_by(&key, &issuer).unwrap().der().clone();
    crate::transport::tls::client_verifier(&authority_certificate(&fx.paths.ca_cert).unwrap())
        .unwrap()
        .verify_client_cert(&leaf, &[], UnixTime::now())
        .unwrap();
    let registered = WorkloadRegistry::register(
        &fx.paths.registry,
        &subject_public_key_info(leaf.as_ref()).unwrap(),
        scopes(&["observe@global"]),
        Some(in_an_hour()),
    )
    .unwrap();
    let registry = WorkloadRegistry::load(&fx.paths.registry).unwrap();
    assert_eq!(
        registry
            .lookup_certificate(leaf.as_ref())
            .map(|credential| credential.id.as_str()),
        Some(registered.id.as_str())
    );
}

/// Re-enrollment swaps credentials in one generation: the new one is live
/// and the old one revoked in the same write. A missing or already-revoked
/// predecessor is not an error; a malformed id is.
#[test]
fn register_replacing_revokes_the_predecessor_in_the_same_generation() {
    let fx = fixture();
    let (old_leaf, old) = enroll(&fx.paths, &["*@global"]);
    let commit_replacing = |replaces: &str| {
        let prepared = prepare_enrollment(&fx.paths, &request(), in_an_hour()).unwrap();
        let leaf = leaf_of(prepared.issued_chain_pem().unwrap());
        let registered = prepared.commit_replacing(
            &fx.paths.registry,
            scopes(&["*@global"]),
            in_an_hour(),
            Some(replaces),
            None,
        );
        (leaf, registered)
    };
    let (new_leaf, new) = commit_replacing(&old.id);
    let new = new.unwrap();
    assert!(new.replaced);
    assert_eq!(new.generation, old.generation + 1, "one write");
    let registry = WorkloadRegistry::load(&fx.paths.registry).unwrap();
    assert!(registry.lookup_certificate(old_leaf.as_ref()).is_none());
    assert!(registry.lookup_certificate(new_leaf.as_ref()).is_some());

    let (_, again) = commit_replacing(&old.id);
    assert!(!again.unwrap().replaced, "already revoked: nothing to swap");
    let (_, absent) = commit_replacing(&credential_id(b"never enrolled"));
    assert!(!absent.unwrap().replaced);
    let (_, malformed) = commit_replacing("sha256:NOT-CANONICAL");
    assert!(matches!(malformed, Err(WorkloadError::InvalidCredentialId)));
}

/// Who a credential is for is recorded at enrollment and survives every
/// rewrite; a record that predates the field is a person's device exactly
/// when it holds the pairing ceiling (ADR-0163 §6).
#[test]
fn a_credential_records_who_it_is_for_and_old_records_infer_it_from_pairing() {
    let fx = fixture();
    let commit = |scope: &str, enrolled_for: Option<EnrolledFor>| {
        prepare_enrollment(&fx.paths, &request(), in_an_hour())
            .unwrap()
            .commit_replacing(
                &fx.paths.registry,
                scopes(&[scope]),
                in_an_hour(),
                None,
                enrolled_for,
            )
            .unwrap()
            .id
    };
    let phone = commit("observe@global", Some(EnrolledFor::Human));
    let bot = commit(PAIRED_DEVICE_SCOPE, Some(EnrolledFor::Automation));
    let old_device = commit(LEGACY_PAIRED_DEVICE_SCOPE, None);
    let old_bot = commit("*@global", None);
    // Another write rewrites every record; nothing recorded is lost.
    let other = commit("observe@host", None);
    WorkloadRegistry::revoke(&fx.paths.registry, &other).unwrap();

    let registry = WorkloadRegistry::load(&fx.paths.registry).unwrap();
    let enrolled_for = |id: &str| registry.lookup(id).unwrap().enrolled_for();
    assert_eq!(enrolled_for(&phone), EnrolledFor::Human, "recorded");
    assert_eq!(
        enrolled_for(&bot),
        EnrolledFor::Automation,
        "recorded beats the ceiling"
    );
    assert_eq!(enrolled_for(&old_device), EnrolledFor::Human, "inferred");
    assert_eq!(enrolled_for(&old_bot), EnrolledFor::Automation, "inferred");

    let text = std::fs::read_to_string(&fx.paths.registry).unwrap();
    let image: serde_json::Value = serde_json::from_str(&text).unwrap();
    let principals: Vec<_> = image["credentials"]
        .as_array()
        .unwrap()
        .iter()
        .map(|record| record.get("principal").cloned())
        .collect();
    assert_eq!(
        principals,
        vec![
            Some(serde_json::json!("human")),
            Some(serde_json::json!("automation")),
            None,
            None,
            None,
        ],
        "an unnamed principal stays unwritten, so older readers still load it"
    );
}

/// Pairing promises full terminal-endpoint control, including organization
/// and keyed termination. Keep its spelled-out ceiling in step with the verbs.
#[test]
fn paired_device_scope_includes_every_terminal_verb() {
    let ceiling = parse_ceiling(&scopes(&[PAIRED_DEVICE_SCOPE])).unwrap();
    let all = parse_ceiling(&scopes(&["*@global"])).unwrap();
    assert_eq!(ceiling, all);
}

/// A ticket enrolls a person's device whatever ceiling it carries.
#[test]
fn a_ticket_enrolls_a_human_principal() {
    let fx = fixture();
    let minted = tickets::mint_ticket(
        &tickets::tickets_path(&fx.paths.registry),
        scopes(&["observe,input@host"]),
        3600,
        600,
    )
    .unwrap();
    let request = ClientRequest::generate().unwrap();
    let csr = ClientMaterial::from_pem(request.csr_pem().as_bytes()).unwrap();
    let ClientMaterial::Request(csr) = csr else {
        panic!("a CSR");
    };
    let enrolled = tickets::enroll_with_ticket(
        &fx.paths,
        &hex::decode(&minted.secret_hex).unwrap(),
        csr.as_ref(),
    )
    .unwrap();
    let registry = WorkloadRegistry::load(&fx.paths.registry).unwrap();
    assert_eq!(
        registry
            .lookup(&enrolled.credential)
            .unwrap()
            .enrolled_for(),
        EnrolledFor::Human
    );
}

/// The client half end to end: the key never leaves the request, the CSR
/// alone is signed, the reply is bound to the key, and the stored files are
/// owner-only and name the credential.
#[test]
fn a_client_request_accepts_its_own_issued_chain_and_stores_it_owner_only() {
    let fx = fixture();
    let request = ClientRequest::generate().unwrap();
    assert!(!request.csr_pem().contains("PRIVATE KEY"));
    let expected = request.credential_id();
    let material = ClientMaterial::from_pem(request.csr_pem().as_bytes()).unwrap();
    let prepared = prepare_enrollment(&fx.paths, &material, in_an_hour()).unwrap();
    assert_eq!(prepared.credential_id(), expected);
    let chain = prepared.issued_chain_pem().unwrap().to_owned();
    let identity = request.accept(&chain).unwrap();
    assert_eq!(identity.credential_id(), expected);
    assert_eq!(
        identity.ca_fingerprint(),
        ca_fingerprint(&fx.paths.ca_cert).unwrap()
    );
    let rendered = format!("{identity:?} {request:?}");
    assert!(rendered.contains("[withheld]"), "{rendered}");

    let remotes = fx.dir.path().join("remotes");
    let (key, cert) = (remotes.join("mini.key"), remotes.join("mini.pem"));
    identity.store(&key, &cert).unwrap();
    for file in [&key, &cert] {
        let mode = std::fs::metadata(file).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
    let key_pem = std::fs::read_to_string(&key).unwrap();
    assert!(key_pem.contains("PRIVATE KEY"));
    assert_eq!(std::fs::read_to_string(&cert).unwrap(), chain);
    assert_eq!(
        stored_credential_id(&cert).as_deref(),
        Some(expected.as_str())
    );
    // The leaf's notAfter is the registry expiry rounded up to the next
    // midnight after a day of slack: never earlier, at most a day later.
    let not_after = stored_certificate_expiry(&cert).unwrap();
    let expires = in_an_hour();
    assert!(
        (expires - 60..=expires + 86_400 + 60).contains(&not_after),
        "{not_after} vs {expires}"
    );
    assert_eq!(
        stored_certificate_expiry(&remotes.join("missing.pem")),
        None
    );
    assert_eq!(
        stored_certificate_expiry(&key),
        None,
        "a key is no certificate"
    );
    assert_eq!(
        stored_chain_authority(&cert).as_deref(),
        Some(identity.ca_fingerprint())
    );
    assert_eq!(stored_chain_authority(&key), None);
    for needle in key_needles(&key_pem) {
        assert!(!rendered.contains(&needle), "{rendered}");
    }
    assert!(matches!(
        identity.store(&key, &cert),
        Err(WorkloadError::AlreadyStored)
    ));
    assert!(matches!(
        identity.store(&remotes.join("other.key"), &fx.dir.path().join("other.pem")),
        Err(WorkloadError::Insecure { .. })
    ));
    remove_identity_files(&key, &cert);
    assert!(!key.exists() && !cert.exists());
}

/// Every reply that is not exactly this key's issued leaf then its CA is
/// refused by rule, and none is echoed.
#[test]
fn a_client_request_refuses_every_other_reply() {
    let fx = fixture();
    let other = fixture();
    let issue = |fx: &Fixture, request: &ClientRequest| {
        let material = ClientMaterial::from_pem(request.csr_pem().as_bytes()).unwrap();
        prepare_enrollment(&fx.paths, &material, in_an_hour())
            .unwrap()
            .issued_chain_pem()
            .unwrap()
            .to_owned()
    };
    let split = |chain: &str| {
        let second = chain[1..].find("-----BEGIN CERTIFICATE-----").unwrap() + 1;
        (chain[..second].to_owned(), chain[second..].to_owned())
    };
    let request = ClientRequest::generate().unwrap();
    let mine = issue(&fx, &request);
    let (leaf, ca) = split(&mine);
    let someone_else = issue(&fx, &ClientRequest::generate().unwrap());
    let other_ca = std::fs::read_to_string(&other.paths.ca_cert).unwrap();
    let key_pem = KeyPair::generate().unwrap().serialize_pem();
    for reply in [
        someone_else,
        format!("{ca}{leaf}"),
        leaf.clone(),
        format!("{mine}{ca}"),
        format!("{leaf}{other_ca}"),
        format!("{mine}{key_pem}"),
        format!("{ca}{ca}"),
        String::new(),
        "not pem at all".to_owned(),
        "x".repeat(MAX_MATERIAL_BYTES + 1),
    ] {
        let error = request.accept(&reply).unwrap_err();
        assert!(matches!(error, WorkloadError::IssuedMismatch(_)), "{error}");
        assert!(!error.to_string().contains("BEGIN"), "{error}");
    }
    assert!(
        request.accept(&mine).is_ok(),
        "the right pair still accepts"
    );
    // Another authority's chain for this key verifies against its own CA:
    // the reply names the CA it is checked against (no CA pin yet, §2).
    assert!(request.accept(&issue(&other, &request)).is_ok());
}

/// A reply the CA really signed, for this request's own key, is still
/// refused when the leaf is itself a CA or is not valid now.
#[test]
fn a_client_request_refuses_a_ca_leaf_and_an_expired_one() {
    let fx = fixture();
    let ca_der = authority_certificate(&fx.paths.ca_cert).unwrap();
    let ca_pem = std::fs::read_to_string(&fx.paths.ca_cert).unwrap();
    let issuer =
        Issuer::from_ca_cert_der(&ca_der, load_authority_key(&fx.paths.ca_key).unwrap()).unwrap();
    let request = ClientRequest::generate().unwrap();
    let sign = |params: CertificateParams| {
        let csr = CertificateSigningRequestParams::from_pem(request.csr_pem()).unwrap();
        let leaf = CertificateSigningRequestParams {
            params,
            public_key: csr.public_key,
        }
        .signed_by(&issuer)
        .unwrap();
        format!("{}{ca_pem}", leaf.pem())
    };
    let client_params = || {
        let mut params = client_certificate_params(in_an_hour(), &request.credential_id()).unwrap();
        params.not_before = rcgen::date_time_ymd(2020, 1, 1);
        params
    };

    let mut ca_leaf = client_params();
    ca_leaf.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    let mut expired = client_params();
    expired.not_after = rcgen::date_time_ymd(2021, 1, 1);
    let mut not_yet = client_params();
    not_yet.not_before = rcgen::date_time_ymd(2999, 1, 1);
    not_yet.not_after = rcgen::date_time_ymd(3000, 1, 1);
    for (what, params) in [
        ("a CA leaf", ca_leaf),
        ("an expired leaf", expired),
        ("a leaf not yet valid", not_yet),
    ] {
        let error = request.accept(&sign(params)).expect_err(what);
        assert!(
            matches!(error, WorkloadError::IssuedMismatch(_)),
            "{what}: {error}"
        );
    }
    assert!(
        request.accept(&sign(client_params())).is_ok(),
        "the same construction with valid fields accepts"
    );
}
