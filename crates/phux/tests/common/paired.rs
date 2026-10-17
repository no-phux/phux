//! A `paired` server and an enrolled client, for suites that dial a remote
//! listener: `phux server` runs one only under `[policy] mode = "paired"`,
//! and admits only a connection presenting an enrolled workload certificate
//! (ADR-0179, `docs/spec/workload-auth.md` §8).

#![allow(
    dead_code,
    reason = "shared integration-test helpers are used per test binary"
)]
#![allow(unreachable_pub, reason = "shared by sibling integration-test crates")]
#![allow(clippy::expect_used, clippy::panic, reason = "test harness")]

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::Output;

/// Append `[policy] mode = "paired"` to the `config.toml` at `config_toml`,
/// creating it. Appended, so top-level keys already there stay top-level.
pub fn write_paired_policy(config_toml: &Path) {
    if let Some(parent) = config_toml.parent() {
        std::fs::create_dir_all(parent).expect("config dir");
    }
    let mut body = std::fs::read_to_string(config_toml).unwrap_or_default();
    if !body.is_empty() && !body.ends_with('\n') {
        body.push('\n');
    }
    body.push_str("[policy]\nmode = \"paired\"\n");
    std::fs::write(config_toml, body).expect("write paired policy");
}

/// A client certificate and key the server's workload registry admits.
#[derive(Debug, Clone)]
pub struct Enrolled {
    /// The issued chain, leaf then CA.
    pub cert: PathBuf,
    /// The owner-only private key.
    pub key: PathBuf,
    /// The registry's `sha256:` id for it.
    pub credential_id: String,
}

impl Enrolled {
    /// `PHUX_WORKLOAD_CERT` / `PHUX_WORKLOAD_KEY`, which a dial with no
    /// entry-named identity presents.
    pub fn env(&self) -> [(&'static str, PathBuf); 2] {
        [
            ("PHUX_WORKLOAD_CERT", self.cert.clone()),
            ("PHUX_WORKLOAD_KEY", self.key.clone()),
        ]
    }

    /// The `[[remote]]` lines that make every dial of that entry present
    /// this identity (`client-cert`, `client-key`).
    pub fn remote_lines(&self) -> String {
        format!(
            "client-cert = \"{}\"\nclient-key = \"{}\"\n",
            self.cert.display(),
            self.key.display()
        )
    }
}

/// Run `command` with `stdin` and collect it, for [`enroll_client`].
pub fn output_with_stdin(mut command: std::process::Command, stdin: &[u8]) -> Output {
    use std::io::Write as _;
    use std::process::Stdio;
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn phux");
    let _ = child.stdin.take().expect("stdin").write_all(stdin);
    child.wait_with_output().expect("wait for phux")
}

/// Create the workload CA if missing and enroll a fresh client key with
/// every verb at `global`, writing the pair under `files`. `phux` runs a
/// `phux` command in the server's environment with the given stdin.
pub fn enroll_client(files: &Path, phux: impl Fn(&[&str], &[u8]) -> Output) -> Enrolled {
    let init = phux(&["workload", "authority", "--init"], b"");
    assert!(
        init.status.success(),
        "authority --init: {}",
        String::from_utf8_lossy(&init.stderr)
    );
    let key = rcgen::KeyPair::generate().expect("client key");
    let csr = rcgen::CertificateParams::new(vec!["e2e-client".to_owned()])
        .expect("params")
        .serialize_request(&key)
        .expect("csr")
        .pem()
        .expect("csr pem");
    let cert_path = files.join("e2e-client.pem");
    let key_path = files.join("e2e-client.key");
    std::fs::write(&key_path, key.serialize_pem()).expect("write client key");
    std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o600))
        .expect("chmod client key");
    let out = phux(
        &[
            "workload",
            "add-key",
            "--json",
            "--cert-out",
            cert_path.to_str().expect("utf-8 path"),
            "--scope",
            "*@global",
        ],
        csr.as_bytes(),
    );
    assert!(
        out.status.success(),
        "add-key: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let doc: serde_json::Value = serde_json::from_slice(&out.stdout).expect("add-key json");
    Enrolled {
        cert: cert_path,
        key: key_path,
        credential_id: doc["credential_id"]
            .as_str()
            .expect("credential_id")
            .to_owned(),
    }
}
