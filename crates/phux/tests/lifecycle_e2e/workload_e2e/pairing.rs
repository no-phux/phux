/// An enrolled certificate dials a workload-mTLS QUIC listener; after
/// `revoke`, the next connection with the same certificate is refused by
/// the same running server.
#[test]
#[ignore = "spawns a real server with a workload-mTLS QUIC listener; runs in the e2e lane"]
fn revoke_marks_the_record_and_a_new_connection_is_refused() {
    let dir = TempDir::new().expect("tempdir");
    prepare_dirs(dir.path());
    paired(dir.path());
    init_authority(dir.path());
    let client = client_key();
    let cert = dir.path().join("client.pem");
    let key = dir.path().join("client.key");
    std::fs::write(&key, &client.key_pem).expect("write client key");
    std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600)).expect("chmod");
    let id = enroll(dir.path(), &client, &cert, &["*@global"]);

    let _server = start_server(dir.path(), &["--quic", listeners::LOOPBACK_ANY_PORT], &[]);
    register_loopback_remote(dir.path());
    let identity = [
        ("PHUX_WORKLOAD_CERT", cert.as_path()),
        ("PHUX_WORKLOAD_KEY", key.as_path()),
    ];

    let whoami = ["whoami", "--remote", REMOTE, "--json"];
    let doc = json_doc(&await_success(dir.path(), &whoami, &identity));
    assert_eq!(doc["credential_id"], id.as_str(), "{doc}");

    let anonymous = phux(dir.path(), &whoami);
    assert!(
        !anonymous.status.success(),
        "a dial without a client certificate is refused: {}",
        text(&anonymous.stdout)
    );

    let revoked = phux(dir.path(), &["workload", "revoke", &id]);
    assert!(revoked.status.success(), "{}", text(&revoked.stderr));
    let refused = phux_with(dir.path(), &whoami, b"", &identity);
    assert!(
        !refused.status.success(),
        "a revoked credential was admitted: {}",
        text(&refused.stdout)
    );

    let listed = json_doc(&phux(dir.path(), &["workload", "list", "--json"]));
    assert_eq!(listed["credentials"][0]["status"], "revoked");
    assert_eq!(listed["registry_generation"], 2);
}

fn bearer_bytes(encoded: &str) -> Vec<u8> {
    encoded
        .as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
        .collect()
}

/// A ticket-store failure must not mint a bearer or replace the old one,
/// regardless of the CLI output mode. A healthy retry still mints both.
#[test]
#[ignore = "spawns a real server with a workload-mTLS QUIC listener; runs in the e2e lane"]
fn pair_enroll_ticket_failure_preserves_credentials_and_output() {
    let dir = TempDir::new().expect("tempdir");
    prepare_dirs(dir.path());
    paired(dir.path());
    let _server = start_server(
        dir.path(),
        &["--quic", listeners::LOOPBACK_ANY_PORT],
        &[("PHUX_WS_SECURE", "1")],
    );
    let socket = dir.path().join("s.sock");
    let socket_arg = socket.to_str().expect("utf-8 socket");
    let first = json_doc(&await_success(
        dir.path(),
        &["--socket", socket_arg, "pair", "--json"],
        &[],
    ));
    let old = first["token"].as_str().expect("bearer");
    let tokens = PathBuf::from(first["tokens_path"].as_str().expect("token store"));
    let before = std::fs::read(&tokens).expect("credential store");
    let registry = dir.path().join("blocked/workload-keys");
    std::fs::create_dir_all(registry.parent().unwrap()).expect("store parent");
    let blocked_ticket = registry.with_file_name("enrollment-tickets");
    std::fs::create_dir(&blocked_ticket).expect("block ticket store with directory");
    for mode in ["--json", "--no-qr"] {
        let out = phux_with(
            dir.path(),
            &[
                "--socket",
                socket_arg,
                "pair",
                "--enroll",
                mode,
                "--replace-token",
                old,
            ],
            b"",
            &[("PHUX_WORKLOAD_KEYS", &registry)],
        );
        assert!(!out.status.success(), "ticket store must refuse");
        assert!(
            out.stdout.is_empty(),
            "failure must not expose a bearer or ticket: {}",
            text(&out.stdout)
        );
        assert!(text(&out.stderr).contains("could not mint an enrollment ticket"));
        assert!(
            !text(&out.stderr).contains(old),
            "old bearer must stay secret"
        );
        assert_eq!(
            std::fs::read(&tokens).unwrap(),
            before,
            "replacement must not run"
        );
        assert!(
            phux_server::auth::TokenStore::load(&tokens)
                .unwrap()
                .verify(&bearer_bytes(old))
        );
    }
    std::fs::remove_dir(&blocked_ticket).expect("unblock tickets");
    let out = phux_with(
        dir.path(),
        &[
            "--socket",
            socket_arg,
            "pair",
            "--enroll",
            "--json",
            "--replace-token",
            old,
        ],
        b"",
        &[("PHUX_WORKLOAD_KEYS", &registry)],
    );
    assert!(out.status.success(), "{}", text(&out.stderr));
    let next = json_doc(&out);
    assert_eq!(next["enrollment_ticket"].as_str().unwrap().len(), 64);
    assert_ne!(next["token"], first["token"]);
    assert!(blocked_ticket.is_file(), "ticket was committed");
    let store = phux_server::auth::TokenStore::load(&tokens).unwrap();
    assert!(!store.verify(&bearer_bytes(old)));
    assert!(store.verify(&bearer_bytes(next["token"].as_str().unwrap())));

    let plain = phux_with(
        dir.path(),
        &["--socket", socket_arg, "pair", "--enroll", "--no-qr"],
        b"",
        &[("PHUX_WORKLOAD_KEYS", &registry)],
    );
    assert!(plain.status.success(), "{}", text(&plain.stderr));
    assert!(text(&plain.stdout).contains("Enrollment ticket t-"));
    assert!(text(&plain.stdout).contains("Pairing token"));
}

/// ADR-0154 end to end through the CLI: a `paired` server mints a ticket
/// with `phux pair --enroll`; a connect code carrying it registers the host
/// and enrolls a key the client generated, which then reaches the paired
/// listener; the same code enrolls nothing a second time.
#[test]
#[ignore = "spawns a real server with a workload-mTLS QUIC listener; runs in the e2e lane"]
fn a_connect_code_with_a_ticket_enrolls_once_and_reaches_a_paired_server() {
    let dir = TempDir::new().expect("tempdir");
    prepare_dirs(dir.path());
    paired(dir.path());
    // Secure, so the loopback listener asks for the bearer token the link
    // carries, as a routable one does.
    let _server = start_server(
        dir.path(),
        &["--quic", listeners::LOOPBACK_ANY_PORT],
        &[("PHUX_WS_SECURE", "1")],
    );
    let socket = dir.path().join("s.sock");
    let socket_arg = socket.to_str().expect("utf-8 socket");
    let paired = json_doc(&await_success(
        dir.path(),
        &["--socket", socket_arg, "pair", "--enroll", "--json"],
        &[],
    ));
    let field = |name: &str| paired[name].as_str().unwrap_or_default().to_owned();
    let ticket = field("enrollment_ticket");
    assert_eq!(ticket.len(), 64, "{paired}");
    let quic = field("quic_addr");
    // A loopback listener gets no device link, so build the one a device
    // would scan, from the same document.
    let link = format!(
        "https://phux.sh/connect?url=wss://127.0.0.1:1&quic=quic://{quic}&fp={}&ca={}&enroll={ticket}&token={}",
        field("cert_fingerprint"),
        field("ca_fingerprint"),
        field("token"),
    );

    let attach = |name: &str| {
        run_in_pty(
            dir.path(),
            &["attach", "--remote", name, "--code", &link, "--no-enroll"],
            &["enrolled a workload certificate", "could not enroll"],
        )
    };
    let first = attach("mini");
    assert!(first.contains("enrolled a workload certificate"), "{first}");
    assert!(!first.contains(&ticket), "the ticket is never echoed");
    // `--code` registers the link's `url`, a placeholder here; point the
    // entry at the QUIC listener it enrolled over.
    let config_path = dir.path().join("config/phux/config.toml");
    let config = std::fs::read_to_string(&config_path).expect("registry");
    assert!(
        config.contains("client-cert"),
        "the identity is recorded: {config}"
    );
    std::fs::write(
        &config_path,
        config.replace("wss://127.0.0.1:1", &format!("quic://{quic}")),
    )
    .expect("rewrite registry");
    let whoami = json_doc(&await_success(
        dir.path(),
        &["whoami", "--remote", "mini", "--json"],
        &[],
    ));
    let listed = json_doc(&phux(dir.path(), &["workload", "list", "--json"]));
    assert_eq!(
        listed["credentials"].as_array().map(Vec::len),
        Some(1),
        "{listed}"
    );
    assert_eq!(
        whoami["credential_id"], listed["credentials"][0]["credential_id"],
        "{whoami}"
    );

    let second = attach("mini2");
    assert!(
        second.contains("could not enroll a workload certificate"),
        "a spent ticket enrolls nothing: {second}"
    );
    let refused = phux(dir.path(), &["whoami", "--remote", "mini2", "--json"]);
    assert!(
        !refused.status.success(),
        "no certificate, no admission: {}",
        text(&refused.stdout)
    );
    let listed = json_doc(&phux(dir.path(), &["workload", "list", "--json"]));
    assert_eq!(listed["credentials"].as_array().map(Vec::len), Some(1));
}
