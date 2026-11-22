/// ADR-0116 two-phase re-enrollment. A reply the client refuses after the
/// far host committed leaves the entry on its working certificate and
/// revokes only the orphan; a good one rewrites the entry first and revokes
/// the old credential after. An entry naming only its certificate, or only
/// its key, still revokes the credential it held, through the enrolled pair
/// on disk, and leaves neither file behind.
#[test]
#[ignore = "spawns a real server with a paired QUIC listener; runs in the e2e lane"]
fn reenrollment_revokes_the_old_certificate_only_after_the_entry_moves() {
    let host = Enrolled::start();
    let entry = host.entry();
    let (cert, key) = (
        entry_path(&entry, "client-cert"),
        entry_path(&entry, "client-key"),
    );

    // The far host enrolls, the client refuses the reply: nothing moves.
    let tamper = host.dir.path().join("tamper-add-key");
    std::fs::write(&tamper, "").expect("tamper");
    host.drop_token();
    let doc = host.add_again();
    std::fs::remove_file(&tamper).expect("untamper");
    let enrollment = &doc["enrollment"];
    assert_eq!(enrollment["status"], "failed", "{doc}");
    assert!(
        enrollment["error"]
            .as_str()
            .is_some_and(|error| error.contains("private key")),
        "{doc}"
    );
    assert_eq!(enrollment["credential_id"], host.first.as_str(), "{doc}");
    assert_eq!(enrollment["previous_revoked"], serde_json::Value::Null);
    assert_eq!(entry_path(&host.entry(), "client-cert"), cert);
    assert!(cert.exists() && key.exists(), "the working pair is kept");
    assert_eq!(host.status(&host.first), "active");
    let credentials = host.far.credentials();
    assert_eq!(credentials.len(), 2, "{credentials:?}");
    assert!(
        credentials.iter().any(
            |credential| credential["credential_id"] != host.first.as_str()
                && credential["status"] == "revoked"
        ),
        "the orphaned credential is revoked: {credentials:?}"
    );
    assert_eq!(host.whoami(), host.first, "still admitted as before");

    // The same re-enrollment untampered: entry first, revocation after.
    host.clear_calls();
    host.drop_token();
    let doc = host.add_again();
    let second = assert_rolled_forward(&host, &doc, &host.first);
    assert!(!cert.exists() && !key.exists(), "the old pair is removed");

    // Half an identity: only the certificate named.
    let entry = host.entry();
    let (cert, key) = (
        entry_path(&entry, "client-cert"),
        entry_path(&entry, "client-key"),
    );
    host.forget("client-key");
    host.clear_calls();
    let doc = host.add_again();
    let third = assert_rolled_forward(&host, &doc, &second);
    assert!(!cert.exists() && !key.exists(), "both halves are removed");

    // Only the key named: the enrolled certificate beside it names the
    // credential.
    let entry = host.entry();
    let (cert, key) = (
        entry_path(&entry, "client-cert"),
        entry_path(&entry, "client-key"),
    );
    host.forget("client-cert");
    host.clear_calls();
    let doc = host.add_again();
    assert_rolled_forward(&host, &doc, &third);
    assert!(!cert.exists() && !key.exists(), "both halves are removed");
}

/// Renewal: a certificate inside the renewal window makes every dial warn
/// and `phux doctor` report it; `host add` on a host that answers renews it
/// instead of calling it done, and `host renew` replaces a certificate on
/// demand without touching the rest of the entry.
#[test]
#[ignore = "spawns a real server with a paired QUIC listener; runs in the e2e lane"]
#[expect(
    clippy::cognitive_complexity,
    reason = "one linear warn-then-renew scenario; every assert! scores as a branch"
)]
fn a_certificate_due_for_renewal_warns_and_is_renewed() {
    let host = Enrolled::start();
    let short = install_short_lived_identity(&host, 5);

    let whoami = host.client(&["whoami", "--remote", REMOTE, "--json"]);
    assert!(whoami.status.success(), "{}", text(&whoami.stderr));
    assert_eq!(json_doc(&whoami)["credential_id"], short.as_str());
    let warned = text(&whoami.stderr);
    assert!(
        warned.contains("expires on") && warned.contains("phux host renew loop"),
        "{warned}"
    );
    let doctor = host.client(&["doctor", "--json"]);
    let doctor: serde_json::Value = serde_json::from_slice(&doctor.stdout).expect("doctor json");
    let check = doctor["checks"]
        .as_array()
        .expect("checks")
        .iter()
        .find(|check| check["name"] == "client-certs")
        .expect("the client-certs check")
        .clone();
    assert_eq!(check["status"], "warn", "{check}");
    assert!(
        check["hint"]
            .as_str()
            .is_some_and(|hint| hint.contains("phux host renew loop")),
        "{check}"
    );

    // The saved route answers, but the certificate is due: renewed.
    host.clear_calls();
    let doc = host.add_again();
    let renewed = assert_rolled_forward(&host, &doc, &short);
    let quiet = host.client(&["whoami", "--remote", REMOTE, "--json"]);
    assert!(
        !text(&quiet.stderr).contains("phux host renew"),
        "{}",
        text(&quiet.stderr)
    );

    // On demand: only the identity changes.
    let before = host.entry();
    host.clear_calls();
    let out = host.client(&["host", "renew", REMOTE, "--json"]);
    assert!(out.status.success(), "host renew: {}", text(&out.stderr));
    let doc = json_doc(&out);
    let newest = assert_rolled_forward(&host, &doc, &renewed);
    let after = host.entry();
    for key in ["endpoint", "token-file", "cert-fingerprint", "ssh"] {
        assert_eq!(before.get(key), after.get(key), "{key}");
    }
    assert!(
        !host.calls().contains("phux pair"),
        "renew does not re-pair"
    );
    assert!(!entry_path(&before, "client-cert").exists());
    assert!(!entry_path(&before, "client-key").exists());
    assert_eq!(
        doc["host"]["client_cert"],
        after["client-cert"].as_str().expect("path")
    );
    let key_pem = std::fs::read_to_string(entry_path(&after, "client-key")).expect("key");
    assert_no_key_bytes(&out, &key_needles(&key_pem));
    assert_eq!(host.whoami(), newest);

    let missing = host.client(&["host", "renew", "nosuch", "--json"]);
    assert!(!missing.status.success());

    // The destination now reaches a host with another authority (here the
    // same host with a fresh CA): the new certificate is refused before the
    // entry moves, and its orphaned credential is revoked.
    for file in ["workload-ca.pem", "workload-ca.key"] {
        std::fs::remove_file(host.far.root.join("state/phux").join(file)).expect("drop CA");
    }
    let reinit = host.far.phux(&["workload", "authority", "--init"]);
    assert!(reinit.status.success(), "{}", text(&reinit.stderr));
    let pinned = host.entry();
    let out = host.client(&["host", "renew", REMOTE, "--json"]);
    assert!(!out.status.success(), "{}", text(&out.stdout));
    assert!(
        text(&out.stderr).contains("different workload authority"),
        "{}",
        text(&out.stderr)
    );
    assert_eq!(host.entry(), pinned, "the entry is unchanged");
    assert!(entry_path(&pinned, "client-cert").exists());
    assert_eq!(host.status(&newest), "active");
    // `first` was swapped out of the entry by hand, never revoked.
    let mut active: Vec<String> = host
        .far
        .credentials()
        .into_iter()
        .filter(|credential| credential["status"] == "active")
        .filter_map(|credential| credential["credential_id"].as_str().map(str::to_owned))
        .collect();
    active.sort();
    let mut expected = vec![host.first, newest];
    expected.sort();
    assert_eq!(active, expected, "the orphan is revoked");
}
