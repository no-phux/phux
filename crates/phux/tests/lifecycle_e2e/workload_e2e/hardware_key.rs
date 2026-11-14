/// The first line of a hardware key file (`phux_client_runtime::hardware_key::HEADER`).
const HARDWARE_KEY_HEADER: &str = "phux-secure-enclave-key-v1";

/// Where `host ls --json` says the remote's client key is.
fn listed_key_kind(host: &Enrolled) -> serde_json::Value {
    let out = host.client(&["host", "ls", "--json"]);
    assert!(out.status.success(), "host ls: {}", text(&out.stderr));
    json_doc(&out)["hosts"][0]["client_key_kind"].clone()
}

/// Every file under the client's `remotes` directory that holds a PEM
/// private key.
fn pem_keys(host: &Enrolled) -> Vec<PathBuf> {
    let remotes = entry_path(&host.entry(), "client-key")
        .parent()
        .expect("remotes dir")
        .to_path_buf();
    std::fs::read_dir(remotes)
        .expect("remotes")
        .filter_map(|entry| Some(entry.ok()?.path()))
        .filter(|path| {
            std::fs::read_to_string(path).is_ok_and(|text| text.contains("PRIVATE KEY"))
        })
        .collect()
}

/// `phux host renew --hardware-key` moves a file key into the Secure
/// Enclave: the entry names the new pair first, the old credential is
/// revoked after, no PEM key is left on disk, and every later dial signs
/// with the enclave. A host on a hardware key stays on one through a plain
/// renew, and an altered key file is refused before any dial.
///
/// A renewal that fails part way keeps the working file key. Where there is
/// no Secure Enclave (Linux, a virtual Mac) the flag is
/// refused before ssh runs and the entry, its files and its admission are
/// exactly as they were.
#[test]
#[ignore = "spawns a real server with a paired QUIC listener; runs in the e2e lane"]
#[expect(
    clippy::cognitive_complexity,
    reason = "one linear fail-then-renew scenario; every assert! scores as a branch"
)]
#[allow(clippy::print_stderr, reason = "says which branch a machine without an enclave took")]
fn renewing_onto_a_hardware_key_leaves_no_pem_behind() {
    let host = Enrolled::start();
    let before = host.entry();
    let (old_cert, old_key) = (
        entry_path(&before, "client-cert"),
        entry_path(&before, "client-key"),
    );
    assert_eq!(listed_key_kind(&host), "file");
    assert_eq!(pem_keys(&host), std::slice::from_ref(&old_key));

    // A renewal that fails after the enclave made its key (the far host's
    // reply is refused) keeps the working file key and stores nothing.
    let tamper = host.dir.path().join("tamper-add-key");
    std::fs::write(&tamper, "").expect("tamper");
    host.clear_calls();
    let failed = host.client(&["host", "renew", REMOTE, "--hardware-key", "--json"]);
    std::fs::remove_file(&tamper).expect("untamper");
    assert!(!failed.status.success(), "{}", text(&failed.stdout));
    let reached_far_host = !host.calls().is_empty();
    assert_eq!(host.entry(), before, "the entry is unchanged");
    assert_eq!(
        pem_keys(&host),
        std::slice::from_ref(&old_key),
        "the file key is kept"
    );
    assert_eq!(listed_key_kind(&host), "file");
    assert_eq!(host.whoami(), host.first, "still admitted as before");
    let keys: Vec<PathBuf> = std::fs::read_dir(old_key.parent().expect("remotes"))
        .expect("remotes")
        .filter_map(|entry| Some(entry.ok()?.path()))
        .filter(|path| path.extension().is_some_and(|extension| extension == "key"))
        .collect();
    assert_eq!(
        keys,
        std::slice::from_ref(&old_key),
        "no other key file is stored"
    );
    if reached_far_host {
        assert!(
            text(&failed.stderr).contains("still presents its current certificate"),
            "{}",
            text(&failed.stderr)
        );
    }

    host.clear_calls();
    let out = host.client(&["host", "renew", REMOTE, "--hardware-key", "--json"]);
    if !out.status.success() {
        assert!(!reached_far_host, "the enclave made a key a moment ago");
        let refused = text(&out.stderr);
        eprintln!("no Secure Enclave here; checking the refusal only: {refused}");
        assert!(refused.contains("Secure Enclave"), "{refused}");
        assert!(refused.contains("nothing was changed"), "{refused}");
        assert_eq!(host.calls(), "", "the far host was never contacted");
        assert_eq!(host.entry(), before, "the entry is unchanged");
        assert!(old_cert.exists() && old_key.exists(), "the pair is kept");
        assert_eq!(host.whoami(), host.first, "still admitted as before");
        return;
    }

    let doc = json_doc(&out);
    let second = assert_rolled_forward(&host, &doc, &host.first);
    assert_eq!(doc["host"]["client_key_kind"], "hardware", "{doc}");
    assert!(!old_cert.exists() && !old_key.exists(), "the old pair is removed");
    assert_eq!(pem_keys(&host), Vec::<PathBuf>::new(), "no PEM key is left");
    let key = entry_path(&host.entry(), "client-key");
    let stored = std::fs::read_to_string(&key).expect("key file");
    assert!(stored.starts_with(HARDWARE_KEY_HEADER), "{stored}");
    let mode = std::fs::metadata(&key).expect("key").permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
    assert_eq!(listed_key_kind(&host), "hardware");
    // Two separate processes: each reloads the key from its file.
    assert_eq!(host.whoami(), second);
    assert_eq!(host.whoami(), second);

    // An altered key file is refused before the dial, naming the file.
    let middle = stored.len() / 2;
    let flipped = if &stored[middle..=middle] == "0" { "1" } else { "0" };
    let mut altered = stored.clone();
    altered.replace_range(middle..=middle, flipped);
    std::fs::write(&key, altered).expect("alter");
    let refused = host.client(&["whoami", "--remote", REMOTE, "--json"]);
    assert!(!refused.status.success(), "{}", text(&refused.stdout));
    assert!(
        text(&refused.stderr).contains("cannot use the stored key"),
        "{}",
        text(&refused.stderr)
    );
    std::fs::write(&key, &stored).expect("restore");

    // A plain renew keeps the key in hardware.
    host.clear_calls();
    let out = host.client(&["host", "renew", REMOTE, "--json"]);
    assert!(out.status.success(), "host renew: {}", text(&out.stderr));
    let third = assert_rolled_forward(&host, &json_doc(&out), &second);
    assert!(!key.exists(), "the replaced hardware key file is removed");
    assert_eq!(listed_key_kind(&host), "hardware");
    assert_eq!(pem_keys(&host), Vec::<PathBuf>::new());
    assert_eq!(host.whoami(), third);
}
