/// ADR-0116 enrollment end to end over a real paired QUIC listener: `phux
/// host add` generates the key here, sends only the CSR over ssh, stores the
/// validated chain owner-only, and records it in `[[remote]]`; the client
/// then authenticates as that credential with no `PHUX_WORKLOAD_*` in its
/// environment. Re-enrolling replaces the pair and revokes the old
/// credential in one registry write. `phux pair revoke` of the enrolled id
/// ends a live connection with `DETACHED { AUTHORIZATION_REVOKED }` and
/// refuses the next one at the transport (workload-auth §7). No key byte
/// reaches argv, stdout, stderr, or the far host.
#[test]
#[ignore = "spawns a real server with a paired QUIC listener; runs in the e2e lane"]
fn host_add_enrolls_a_certificate_a_paired_listener_admits_until_revoked() {
    let dir = TempDir::new().expect("tempdir");
    prepare_dirs(&dir.path().join("client"));
    let far = FarHost::new(dir.path());
    let ssh = far.fake_ssh(dir.path());
    // Bound on every address, so the listener is a secure one that asks for
    // the pairing token as a real remote's does; dialed on loopback.
    let _server = far.serve("0.0.0.0:0");
    let port = bound_quic_port(&far.root.join("s.sock"));
    let quic = format!("127.0.0.1:{port}");

    let add = [
        "host",
        "add",
        "me@127.0.0.1",
        "--name",
        REMOTE,
        "--endpoint",
        quic.as_str(),
        "--no-service",
        "--json",
    ];
    let (entry, first) = assert_first_enrollment(dir.path(), &far, &ssh, &add, &quic);
    let whoami = ["whoami", "--remote", REMOTE, "--json"];
    let doc = json_doc(&client_phux(dir.path(), &ssh, &whoami));
    assert_eq!(doc["credential_id"], first.as_str(), "{doc}");

    let (entry, second) = assert_reenrollment(dir.path(), &far, &ssh, &add, &entry, &first);
    let doc = json_doc(&client_phux(dir.path(), &ssh, &whoami));
    assert_eq!(doc["credential_id"], second.as_str(), "{doc}");

    // `phux pair revoke` covers enrolled certificates: a live connection
    // with that certificate ends, and the next one is refused.
    let mut live = RawSession::open(&entry, port);
    live.await_hello_ok();
    let revoked = far.phux(&["pair", "revoke", &second]);
    assert!(revoked.status.success(), "{}", text(&revoked.stderr));
    assert_revoked(&mut live);
    let refused = client_phux(dir.path(), &ssh, &whoami);
    assert!(
        !refused.status.success(),
        "a revoked certificate was admitted: {}",
        text(&refused.stdout)
    );
    let mut next = RawSession::open(&entry, port);
    let frames = next.frames_until_close();
    assert!(
        frames.is_empty(),
        "a refused connection gets no frame: {frames:?}"
    );
    let reason = next.close_reason();
    assert!(
        reason.contains("unauthorized"),
        "refused as unauthorized: {reason}"
    );
}

/// The first `host add`: the pair is stored owner-only and recorded, only
/// the CSR crossed ssh, and the far host holds one credential at the
/// `host add` ceiling. Returns the entry and that credential's id.
fn assert_first_enrollment(
    dir: &Path,
    far: &FarHost,
    ssh: &Path,
    add: &[&str],
    quic: &str,
) -> (toml::Value, String) {
    let added = client_phux(dir, ssh, add);
    assert!(added.status.success(), "host add: {}", text(&added.stderr));
    let entry = remote_entry(dir, REMOTE);
    assert_eq!(
        entry["endpoint"].as_str(),
        Some(format!("quic://{quic}").as_str()),
        "the direct route answered with the enrolled certificate: {}\nfar server: {}",
        text(&added.stdout),
        far.server_log()
    );
    let (cert, key) = (
        entry_path(&entry, "client-cert"),
        entry_path(&entry, "client-key"),
    );
    for file in [&cert, &key] {
        let mode = std::fs::metadata(file).expect("stat").permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "{}", file.display());
    }
    let key_pem = std::fs::read_to_string(&key).expect("client key");
    let needles = key_needles(&key_pem);
    assert_no_key_bytes(&added, &needles);
    let calls = std::fs::read_to_string(dir.join("ssh-calls")).expect("ssh calls");
    assert!(
        calls.contains("workload add-key --json --cert-stdout"),
        "the CSR went to add-key: {calls}"
    );
    // Everything that crossed to the far host, and what it kept.
    let sent = std::fs::read_to_string(dir.join("ssh-stdin")).expect("ssh stdin");
    assert!(
        sent.contains("CERTIFICATE REQUEST"),
        "the CSR went on stdin"
    );
    assert!(!sent.contains("PRIVATE KEY"), "only the CSR went on stdin");
    let registry = std::fs::read_to_string(far.root.join("state/phux/workload-keys"))
        .expect("the far registry");
    let far_state = format!("{calls}{sent}{registry}");
    for needle in &needles {
        assert!(
            !far_state.contains(needle.as_str()),
            "key bytes left this machine"
        );
    }
    assert!(!calls.contains("PRIVATE KEY"));

    let enrolled = far.credentials();
    assert_eq!(enrolled.len(), 1, "{enrolled:?}");
    let first = enrolled[0]["credential_id"]
        .as_str()
        .expect("id")
        .to_owned();
    assert_eq!(
        enrolled[0]["scopes"],
        serde_json::json!(["inventory,observe,create,bind,input,signal@global"])
    );
    (entry, first)
}

/// Re-enrollment: with the saved route unusable (its token file gone),
/// `host add` pairs again and enrolls a new certificate, which replaces the
/// old one on the far host in one write and on disk here. Returns the new
/// entry and credential id.
fn assert_reenrollment(
    dir: &Path,
    far: &FarHost,
    ssh: &Path,
    add: &[&str],
    entry: &toml::Value,
    first: &str,
) -> (toml::Value, String) {
    let (cert, key) = (
        entry_path(entry, "client-cert"),
        entry_path(entry, "client-key"),
    );
    std::fs::remove_file(entry_path(entry, "token-file")).expect("drop token");
    let again = client_phux(dir, ssh, add);
    assert!(
        again.status.success(),
        "host add again: {}",
        text(&again.stderr)
    );
    let entry = remote_entry(dir, REMOTE);
    let second_cert = entry_path(&entry, "client-cert");
    assert_ne!(second_cert, cert, "a new pair, never written over the old");
    assert!(!cert.exists() && !key.exists(), "the old pair is removed");
    let listed = far.credentials();
    let status = |id: &str| {
        listed
            .iter()
            .find(|credential| credential["credential_id"] == id)
            .map(|credential| credential["status"].clone())
    };
    assert_eq!(
        status(first),
        Some(serde_json::json!("revoked")),
        "{listed:?}"
    );
    let second = listed
        .iter()
        .find(|credential| credential["status"] == "active")
        .and_then(|credential| credential["credential_id"].as_str())
        .expect("the new credential is active")
        .to_owned();
    assert_ne!(second, first);
    (entry, second)
}

/// The live connection ended: the server closed it (not a read timeout), and
/// the `DETACHED` it flushes first, best effort (workload-auth §7), names the
/// revocation when it arrives before the close.
fn assert_revoked(live: &mut RawSession) {
    use phux_protocol::wire::frame::{DetachReason, FrameKind};
    let frames = live.frames_until_close();
    let reason = live.close_reason();
    assert!(
        reason.starts_with("Some("),
        "the server closed the revoked connection: {reason}; frames: {frames:?}"
    );
    for frame in &frames {
        if let FrameKind::Detached { reason, .. } = frame {
            assert_eq!(
                *reason,
                Some(DetachReason::AuthorizationRevoked),
                "{frames:?}"
            );
        }
    }
}

/// A far host with a paired listener and the first `host add` done: the
/// sandbox, the fake ssh, the server, the `host add --json` argv, and the
/// first credential.
struct Enrolled {
    dir: TempDir,
    far: FarHost,
    ssh: PathBuf,
    _server: Server,
    add: Vec<String>,
    first: String,
}

impl Enrolled {
    fn start() -> Self {
        let dir = TempDir::new().expect("tempdir");
        prepare_dirs(&dir.path().join("client"));
        let far = FarHost::new(dir.path());
        let ssh = far.fake_ssh(dir.path());
        let server = far.serve("0.0.0.0:0");
        let port = bound_quic_port(&far.root.join("s.sock"));
        let quic = format!("127.0.0.1:{port}");
        let add: Vec<String> = [
            "host",
            "add",
            "me@127.0.0.1",
            "--name",
            REMOTE,
            "--endpoint",
            quic.as_str(),
            "--no-service",
            "--json",
        ]
        .map(str::to_owned)
        .to_vec();
        let argv: Vec<&str> = add.iter().map(String::as_str).collect();
        let (_, first) = assert_first_enrollment(dir.path(), &far, &ssh, &argv, &quic);
        Self {
            dir,
            far,
            ssh,
            _server: server,
            add,
            first,
        }
    }

    fn client(&self, args: &[&str]) -> Output {
        client_phux(self.dir.path(), &self.ssh, args)
    }

    /// `host add` again, as a JSON document; it must succeed.
    fn add_again(&self) -> serde_json::Value {
        let argv: Vec<&str> = self.add.iter().map(String::as_str).collect();
        let out = self.client(&argv);
        assert!(out.status.success(), "host add: {}", text(&out.stderr));
        json_doc(&out)
    }

    fn entry(&self) -> toml::Value {
        remote_entry(self.dir.path(), REMOTE)
    }

    /// The credential the entry's certificate authenticates as, per the far
    /// server.
    fn whoami(&self) -> String {
        let out = self.client(&["whoami", "--remote", REMOTE, "--json"]);
        assert!(out.status.success(), "whoami: {}", text(&out.stderr));
        json_doc(&out)["credential_id"]
            .as_str()
            .expect("credential_id")
            .to_owned()
    }

    fn status(&self, id: &str) -> serde_json::Value {
        self.far
            .credentials()
            .iter()
            .find(|credential| credential["credential_id"] == id)
            .map(|credential| credential["status"].clone())
            .unwrap_or_default()
    }

    /// Make the saved route unusable so `host add` re-pairs.
    fn drop_token(&self) {
        std::fs::remove_file(entry_path(&self.entry(), "token-file")).expect("drop token");
    }

    fn calls(&self) -> String {
        std::fs::read_to_string(self.dir.path().join("ssh-calls")).unwrap_or_default()
    }

    fn clear_calls(&self) {
        std::fs::write(self.dir.path().join("ssh-calls"), "").expect("clear ssh calls");
    }

    fn config_path(&self) -> PathBuf {
        self.dir.path().join("client/config/phux/config.toml")
    }

    /// Drop one key from the `[[remote]]` entry, leaving half an identity.
    fn forget(&self, key: &str) {
        let raw = std::fs::read_to_string(self.config_path()).expect("client config");
        let prefix = format!("{key} =");
        let kept: Vec<&str> = raw
            .lines()
            .filter(|line| !line.trim_start().starts_with(&prefix))
            .collect();
        assert_ne!(kept.len(), raw.lines().count(), "{key} was in the entry");
        std::fs::write(self.config_path(), kept.join("\n") + "\n").expect("rewrite config");
    }
}

/// The `enrollment` object's credential moved from `previous` to a new one,
/// and the far host revoked `previous` only after enrolling its successor.
/// Returns the new credential id.
fn assert_rolled_forward(host: &Enrolled, doc: &serde_json::Value, previous: &str) -> String {
    let enrollment = &doc["enrollment"];
    assert_eq!(enrollment["status"], "enrolled", "{doc}");
    assert_eq!(enrollment["error"], serde_json::Value::Null, "{doc}");
    assert_eq!(enrollment["previous_credential_id"], previous, "{doc}");
    assert_eq!(enrollment["previous_revoked"], true, "{doc}");
    assert_eq!(enrollment["warnings"], serde_json::json!([]), "{doc}");
    let current = enrollment["credential_id"]
        .as_str()
        .expect("credential_id")
        .to_owned();
    assert_ne!(current, previous);
    assert!(enrollment["expires_at"].is_i64(), "{doc}");
    assert_eq!(host.status(previous), "revoked");
    assert_eq!(host.status(&current), "active");
    assert_eq!(host.whoami(), current);
    // The far host enrolled first and revoked after: never `--replace`.
    let calls = host.calls();
    let enrolled_at = calls.find("workload add-key").expect("add-key ran");
    let revoked_at = calls
        .find(&format!("workload revoke {previous}"))
        .expect("the previous credential was revoked");
    assert!(enrolled_at < revoked_at, "{calls}");
    assert!(!calls.contains("--replace "), "{calls}");
    current
}
