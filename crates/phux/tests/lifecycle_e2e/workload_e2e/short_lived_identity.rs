/// Swap the entry's identity for one the far host issued for `days`: a
/// client key and CSR made here, signed by `add-key --expires-in`, stored
/// under the names `host add` gives an enrolled pair. Returns its id.
fn install_short_lived_identity(host: &Enrolled, days: i64) -> String {
    let client = client_key();
    let chain = host.far.root.join("short.pem");
    let seconds = (days * 86_400).to_string();
    let mut far = common::phux_cmd(crate::runner::phux_bin())
        .envs(host.far.env())
        .current_dir(&host.far.root)
        .args([
            "workload",
            "add-key",
            "--json",
            "--scope",
            "inventory,observe,create,bind,input,signal@global",
            "--expires-in",
            &seconds,
            "--cert-out",
            chain.to_str().expect("utf-8"),
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("far add-key");
    far.stdin
        .take()
        .expect("stdin")
        .write_all(client.csr_pem.as_bytes())
        .expect("csr");
    let out = far.wait_with_output().expect("far add-key");
    assert!(out.status.success(), "{}", text(&out.stderr));
    let id = json_doc(&out)["credential_id"]
        .as_str()
        .expect("id")
        .to_owned();

    let entry = host.entry();
    let (old_cert, old_key) = (
        entry_path(&entry, "client-cert"),
        entry_path(&entry, "client-key"),
    );
    let remotes = old_cert.parent().expect("remotes dir");
    let digest = id.strip_prefix("sha256:").expect("canonical id");
    let stem = format!("{REMOTE}.client.{}", &digest[..16]);
    let (cert, key) = (
        remotes.join(format!("{stem}.pem")),
        remotes.join(format!("{stem}.key")),
    );
    for (path, bytes) in [
        (&cert, std::fs::read(&chain).expect("chain")),
        (&key, client.key_pem.into_bytes()),
    ] {
        std::fs::write(path, bytes).expect("write identity");
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).expect("chmod");
    }
    let raw = std::fs::read_to_string(host.config_path()).expect("config");
    let raw = raw
        .replace(
            old_cert.to_str().expect("utf-8"),
            cert.to_str().expect("utf-8"),
        )
        .replace(
            old_key.to_str().expect("utf-8"),
            key.to_str().expect("utf-8"),
        );
    std::fs::write(host.config_path(), raw).expect("rewrite config");
    id
}
