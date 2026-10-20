/// `authority --init` creates the CA and prints its fingerprint and nothing
/// else; the key it wrote is owner-only and never appears in any output.
#[test]
#[ignore = "runs the real binary; runs in the e2e lane"]
fn authority_init_prints_only_a_fingerprint() {
    let dir = TempDir::new().expect("tempdir");
    prepare_dirs(dir.path());

    let missing = phux(dir.path(), &["workload", "authority"]);
    assert!(!missing.status.success(), "no CA exists yet");
    assert!(
        text(&missing.stderr).contains("authority --init"),
        "{}",
        text(&missing.stderr)
    );

    let out = phux(dir.path(), &["workload", "authority", "--init"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    let stdout = text(&out.stdout);
    let fingerprint = stdout.strip_suffix('\n').expect("newline-terminated");
    assert!(!fingerprint.contains('\n'), "exactly one line: {stdout}");
    let digest = fingerprint.strip_prefix("sha256:").expect("sha256: prefix");
    assert!(
        digest.len() == 64
            && digest
                .bytes()
                .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')),
        "{fingerprint}"
    );

    let key = find(dir.path(), "workload-ca.key").expect("CA key written");
    let key_pem = std::fs::read_to_string(&key).expect("read CA key");
    let mode = std::fs::metadata(&key).expect("stat").permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
    assert_no_key_bytes(&out, &key_needles(&key_pem));
    assert!(!text(&out.stderr).contains("workload-ca.key"));

    // Idempotent: the same authority, never a replacement.
    let again = json_doc(&phux(
        dir.path(),
        &["workload", "authority", "--init", "--json"],
    ));
    assert_eq!(again["ca_fingerprint"], fingerprint);
    assert_eq!(again["created"], false);
    assert_eq!(
        text(&phux(dir.path(), &["workload", "authority"]).stdout),
        stdout
    );
    assert_eq!(std::fs::read_to_string(&key).expect("re-read"), key_pem);
}

/// `authority --rotate` (ADR-0153) replaces the CA, reports both
/// fingerprints and the re-pair it forces, keeps the old pair retired
/// owner-only, and prints no key bytes, old or new.
#[test]
#[ignore = "runs the real binary; runs in the e2e lane"]
fn authority_rotate_replaces_the_ca_and_prints_only_fingerprints() {
    let dir = TempDir::new().expect("tempdir");
    prepare_dirs(dir.path());
    let nothing = phux(dir.path(), &["workload", "authority", "--rotate"]);
    assert!(!nothing.status.success(), "nothing to rotate yet");
    assert!(
        text(&nothing.stderr).contains("authority --init"),
        "{}",
        text(&nothing.stderr)
    );

    let original = init_authority(dir.path());
    let key = find(dir.path(), "workload-ca.key").expect("CA key");
    let old_key = std::fs::read_to_string(&key).expect("old key");

    let out = phux(dir.path(), &["workload", "authority", "--rotate", "--json"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    let doc = json_doc(&out);
    assert_eq!(doc["rotated"], true);
    assert_eq!(doc["previous_ca_fingerprint"], original.as_str());
    let rotated = doc["ca_fingerprint"]
        .as_str()
        .expect("new fingerprint")
        .to_owned();
    assert_ne!(rotated, original);
    assert!(
        doc["server_cert_fingerprint"].is_null(),
        "no server pair yet"
    );
    assert_eq!(
        text(&phux(dir.path(), &["workload", "authority"]).stdout).trim(),
        rotated
    );

    let new_key = std::fs::read_to_string(&key).expect("new key");
    let mut needles = key_needles(&old_key);
    needles.extend(key_needles(&new_key));
    assert_no_key_bytes(&out, &needles);
    let suffix = doc["retired_suffix"].as_str().expect("suffix");
    let retired = key.with_file_name(format!("workload-ca.key.{suffix}"));
    assert_eq!(std::fs::read_to_string(&retired).expect("retired"), old_key);
    let mode = std::fs::metadata(&retired)
        .expect("stat")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o600);

    let prose = phux(dir.path(), &["workload", "authority", "--rotate"]);
    assert!(prose.status.success(), "{}", text(&prose.stderr));
    let stdout = text(&prose.stdout);
    assert!(
        stdout.contains(&rotated)
            && stdout.contains("phux host add")
            && stdout.contains("phux upgrade"),
        "names the replaced CA and the re-pair: {stdout}"
    );
    assert_no_key_bytes(
        &prose,
        &key_needles(&std::fs::read_to_string(&key).expect("key")),
    );
}

/// A CSR piped on stdin is signed and enrolled; `list` shows the ceiling,
/// and public keys only when asked.
#[test]
#[ignore = "runs the real binary; runs in the e2e lane"]
fn add_key_from_stdin_enrolls_and_list_shows_the_ceiling() {
    let dir = TempDir::new().expect("tempdir");
    prepare_dirs(dir.path());
    init_authority(dir.path());
    let client = client_key();
    let cert_out = dir.path().join("client.pem");

    let out = phux_with(
        dir.path(),
        &[
            "workload",
            "add-key",
            "--scope",
            "observe,input@host",
            "--scope",
            "inventory@global",
            "--cert-out",
            cert_out.to_str().expect("utf-8"),
        ],
        client.csr_pem.as_bytes(),
        &[],
    );
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_no_key_bytes(&out, &key_needles(&client.key_pem));
    let stdout = text(&out.stdout);
    let id = stdout
        .lines()
        .find_map(|line| line.strip_prefix("Enrolled "))
        .expect("the credential id")
        .to_owned();
    assert!(
        !stdout.contains(cert_out.to_str().expect("utf-8")),
        "user-supplied paths are not echoed: {stdout}"
    );
    let chain = std::fs::read_to_string(&cert_out).expect("issued certificate");
    assert_eq!(chain.matches("BEGIN CERTIFICATE").count(), 2, "leaf and CA");

    let listed = json_doc(&phux(dir.path(), &["workload", "list", "--json"]));
    assert_eq!(listed["registry_generation"], 1);
    assert!(listed["registry_instance"].is_string(), "{listed}");
    let record = &listed["credentials"][0];
    assert_eq!(record["credential_id"], id.as_str());
    assert_eq!(record["status"], "active");
    assert_eq!(
        record["scopes"],
        serde_json::json!(["observe,input@host", "inventory@global"])
    );
    assert!(record["expires_at"].is_i64());
    assert!(
        record.get("public_key").is_none(),
        "public keys only on request"
    );

    let with_keys = json_doc(&phux(
        dir.path(),
        &["workload", "list", "--public-keys", "--json"],
    ));
    assert!(with_keys["credentials"][0]["public_key"].is_string());
    let prose = text(&phux(dir.path(), &["workload", "list"]).stdout);
    assert!(prose.contains(&id) && prose.contains("active"), "{prose}");

    // The same key again is refused and writes nothing.
    let again_out = dir.path().join("again.pem");
    let again = phux_with(
        dir.path(),
        &[
            "workload",
            "add-key",
            "--scope",
            "observe@global",
            "--cert-out",
            again_out.to_str().expect("utf-8"),
        ],
        client.csr_pem.as_bytes(),
        &[],
    );
    assert!(!again.status.success());
    assert!(
        text(&again.stderr).contains("already enrolled"),
        "{}",
        text(&again.stderr)
    );
    assert!(
        !again_out.exists(),
        "a refused enrollment leaves no certificate"
    );

    let bad_scope = phux_with(
        dir.path(),
        &["workload", "add-key", "--scope", "terminal.control"],
        client.csr_pem.as_bytes(),
        &[],
    );
    assert!(!bad_scope.status.success());
    assert!(text(&bad_scope.stderr).contains("workload scope 1"));
}

/// Key material handed over argv, as a `--file` or `--cert-out` value
/// (separate or `=`-joined), as a scope, or as a bare base64 line is
/// refused, and none of it comes back out.
#[test]
#[ignore = "runs the real binary; runs in the e2e lane"]
fn add_key_refuses_key_material_on_argv() {
    let dir = TempDir::new().expect("tempdir");
    prepare_dirs(dir.path());
    init_authority(dir.path());
    let client = client_key();
    let needles = key_needles(&client.key_pem);
    let key = client.key_pem.as_str();
    let base64_line = client
        .key_pem
        .lines()
        .nth(1)
        .expect("a base64 body line of the key");
    let cert_out_joined = format!("--cert-out={key}");
    let cert_out_base64 = format!("--cert-out={base64_line}");

    for args in [
        vec!["workload", "add-key", "--scope", "observe@global", key],
        vec![
            "workload",
            "add-key",
            "--scope",
            "observe@global",
            "--file",
            key,
        ],
        vec!["workload", "add-key", "--scope", key],
        vec!["workload", "revoke", key],
        vec![
            "workload",
            "add-key",
            "--json",
            "--scope",
            "observe@global",
            key,
        ],
        vec![
            "workload",
            "add-key",
            "--scope",
            "observe@global",
            &cert_out_joined,
        ],
        vec![
            "workload",
            "add-key",
            "--scope",
            "observe@global",
            base64_line,
        ],
        vec![
            "workload",
            "add-key",
            "--scope",
            "observe@global",
            &cert_out_base64,
        ],
        vec!["workload", "revoke", base64_line],
    ] {
        let out = phux(dir.path(), &args);
        assert!(!out.status.success(), "refused: {args:?}");
        assert_no_key_bytes(&out, &needles);
    }

    // The reviewer's case: a key as the `--cert-out=` value with a real CSR
    // on stdin.
    let joined = phux_with(
        dir.path(),
        &[
            "workload",
            "add-key",
            "--scope",
            "observe@global",
            &cert_out_joined,
        ],
        client.csr_pem.as_bytes(),
        &[],
    );
    assert!(!joined.status.success());
    assert_no_key_bytes(&joined, &needles);
    assert!(
        find(dir.path(), "workload-keys").is_none(),
        "nothing was enrolled"
    );
}

/// A private key piped on stdin, alone or after a CSR, is refused without
/// being echoed, and enrolls nothing.
#[test]
#[ignore = "runs the real binary; runs in the e2e lane"]
fn add_key_refuses_key_material_on_stdin() {
    let dir = TempDir::new().expect("tempdir");
    prepare_dirs(dir.path());
    init_authority(dir.path());
    let client = client_key();
    let needles = key_needles(&client.key_pem);
    let cert_out = dir.path().join("client.pem");
    let cert_out = cert_out.to_str().expect("utf-8");
    for input in [
        client.key_pem.clone(),
        format!("{}{}", client.csr_pem, client.key_pem),
    ] {
        let out = phux_with(
            dir.path(),
            &[
                "workload",
                "add-key",
                "--scope",
                "observe@global",
                "--cert-out",
                cert_out,
            ],
            input.as_bytes(),
            &[],
        );
        assert!(!out.status.success());
        assert!(
            text(&out.stderr).contains("private key"),
            "{}",
            text(&out.stderr)
        );
        assert!(out.stdout.is_empty());
        assert_no_key_bytes(&out, &needles);
    }
    assert!(
        find(dir.path(), "workload-keys").is_none(),
        "nothing was enrolled"
    );
}

/// A base64url secret (a JWK `d`) carries no PEM marker, so it is caught by
/// the usage-error redaction instead, as an extra `workload` argument and as
/// the naked `phux` target alike.
#[test]
#[ignore = "runs the real binary; runs in the e2e lane"]
fn usage_errors_never_echo_a_base64url_secret() {
    const SECRET: &str = "kG-fUyzXcIZ2qVoMkH7Se-XnBIgz8qr4is4e0PFiWQ0";
    let dir = TempDir::new().expect("tempdir");
    prepare_dirs(dir.path());
    for args in [
        vec!["workload", "add-key", "--scope", "observe@global", SECRET],
        vec![SECRET],
    ] {
        let out = phux(dir.path(), &args);
        assert!(!out.status.success(), "refused: {args:?}");
        let all = format!("{}{}", text(&out.stdout), text(&out.stderr));
        assert!(!all.contains(SECRET), "the secret was echoed: {all}");
    }
}
