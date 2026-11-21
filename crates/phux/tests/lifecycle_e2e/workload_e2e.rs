//! `phux workload` (ADR-0116, `docs/remote.md#who-can-connect`): the
//! authority prints only its fingerprint and no key bytes reach stdout or
//! stderr; a revoked credential's next connection is refused with no restart;
//! and the startup refusals (ADR-0179).

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]

use crate::{common, listeners};

use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Output, Stdio};
use std::time::{Duration, Instant};

use tempfile::TempDir;

use crate::hermetic::{
    NO_ENV, READY_DEADLINE, Server, await_success, hermetic_env, json_doc, phux, phux_with,
    prepare_dirs, server_command, start_server, text,
};

/// The registry name the loopback listener is registered under.
const REMOTE: &str = "loop";

/// A workload's private key and a CSR for it, generated the way a workload
/// would, outside phux.
struct ClientKey {
    key_pem: String,
    csr_pem: String,
}

fn client_key() -> ClientKey {
    let key = rcgen::KeyPair::generate().expect("generate client key");
    let csr = rcgen::CertificateParams::new(vec!["workload".to_owned()])
        .expect("params")
        .serialize_request(&key)
        .expect("csr");
    ClientKey {
        key_pem: key.serialize_pem(),
        csr_pem: csr.pem().expect("csr pem"),
    }
}

/// 16-character slices of a PEM key's base64 body. None may appear in any
/// output.
fn key_needles(pem: &str) -> Vec<String> {
    let body: String = pem
        .lines()
        .filter(|line| !line.starts_with("-----"))
        .collect();
    body.as_bytes()
        .as_chunks::<16>()
        .0
        .iter()
        .map(|chunk| String::from_utf8(chunk.to_vec()).expect("base64 is ascii"))
        .collect()
}

fn assert_no_key_bytes(out: &Output, needles: &[String]) {
    let all = format!("{}{}", text(&out.stdout), text(&out.stderr));
    for needle in needles {
        assert!(
            !all.contains(needle.as_str()),
            "key bytes reached the output: {all}"
        );
    }
}

/// The first file named `name` anywhere under `root`.
fn find(root: &Path, name: &str) -> Option<PathBuf> {
    for entry in std::fs::read_dir(root).ok()?.flatten() {
        let path = entry.path();
        if path.file_name().is_some_and(|file| file == name) {
            return Some(path);
        }
        if path.is_dir()
            && let Some(found) = find(&path, name)
        {
            return Some(found);
        }
    }
    None
}

fn init_authority(dir: &Path) -> String {
    let out = phux(dir, &["workload", "authority", "--init"]);
    assert!(
        out.status.success(),
        "authority --init: {}",
        text(&out.stderr)
    );
    text(&out.stdout).trim().to_owned()
}

/// Enroll `client`'s CSR through `add-key` on stdin; the credential id.
fn enroll(dir: &Path, client: &ClientKey, cert_out: &Path, scopes: &[&str]) -> String {
    let mut args = vec![
        "workload",
        "add-key",
        "--json",
        "--cert-out",
        cert_out.to_str().expect("utf-8 path"),
    ];
    for scope in scopes {
        args.extend(["--scope", scope]);
    }
    let out = phux_with(dir, &args, client.csr_pem.as_bytes(), NO_ENV);
    assert!(out.status.success(), "add-key: {}", text(&out.stderr));
    json_doc(&out)["credential_id"]
        .as_str()
        .expect("credential_id")
        .to_owned()
}

/// The port the QUIC listener of the server on `socket` bound.
fn bound_quic_port(socket: &Path) -> u16 {
    listeners::bound_listener_addr(socket, listeners::RemoteListenerTransport::Quic).port()
}

/// Register the server on `dir`'s socket as [`REMOTE`], by the QUIC port it
/// bound.
fn register_loopback_remote(dir: &Path) {
    let port = bound_quic_port(&dir.join("s.sock"));
    let config = dir.join("config/phux/config.toml");
    let body = format!(
        "{}[[remote]]\nname = \"{REMOTE}\"\nendpoint = \"quic://127.0.0.1:{port}\"\n",
        std::fs::read_to_string(&config).unwrap_or_default()
    );
    std::fs::write(config, body).expect("write registry");
}

/// `[policy] mode = "paired"` in the hermetic config, the one switch that
/// serves enrolled workloads (`PHUX_WORKLOAD_MTLS` is
/// retired, ADR-0179).
fn paired(dir: &Path) {
    std::fs::write(
        dir.join("config/phux/config.toml"),
        "[policy]\nmode = \"paired\"\n",
    )
    .expect("write paired config");
}

// Lexical shards retain test paths and private helper access without widening
// visibility. Keep whole items (including docs and attributes) together.
include!("workload_e2e/authority.rs");
include!("workload_e2e/paired_repair.rs");
include!("workload_e2e/pairing.rs");
include!("workload_e2e/secrets_and_startup.rs");
include!("workload_e2e/approvals.rs");
include!("workload_e2e/host_harness.rs");
include!("workload_e2e/raw_session.rs");
include!("workload_e2e/host_enrollment.rs");
include!("workload_e2e/renewal.rs");
include!("workload_e2e/hardware_key.rs");
include!("workload_e2e/host_read.rs");
include!("workload_e2e/narrow_read.rs");
include!("workload_e2e/short_lived_identity.rs");
