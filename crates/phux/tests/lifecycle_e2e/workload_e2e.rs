//! `phux workload` against the real binary and a real server (ADR-0116,
//! `docs/spec/workload-auth.md` §8).
//!
//! The CLI tests run hermetically against a private state directory and pin
//! the secret-hygiene rules: the authority prints only its fingerprint, and
//! no key bytes reach stdout or stderr however they are handed in. The
//! server tests start a server whose loopback QUIC listener requires
//! workload mTLS, dial it with a certificate `add-key` issued, revoke that
//! credential, and see the next connection refused with no restart; and
//! they pin that workload mode refuses to start beside an entry point that
//! cannot carry a client certificate.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]

#[path = "../common/ambient.rs"]
mod common;

use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Output, Stdio};
use std::time::{Duration, Instant};

use tempfile::TempDir;

const PHUX: &str = env!("CARGO_BIN_EXE_phux");

/// The registry name the loopback listener is registered under.
const REMOTE: &str = "loop";

/// How long a freshly started server has to answer its first dial.
const READY_DEADLINE: Duration = Duration::from_secs(30);

/// A running `phux server`, killed on drop.
struct Server(Child);

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn hermetic_env(dir: &Path) -> Vec<(&'static str, PathBuf)> {
    vec![
        ("XDG_CONFIG_HOME", dir.join("config")),
        ("XDG_STATE_HOME", dir.join("state")),
        ("XDG_RUNTIME_DIR", dir.join("run")),
        ("PHUX_PROFILE", PathBuf::from("default")),
        ("PHUX_SSH", dir.join("no-such-ssh")),
        ("PHUX_TAILSCALE", dir.join("no-such-tailscale")),
    ]
}

fn prepare_dirs(dir: &Path) {
    std::fs::create_dir_all(dir.join("config/phux")).expect("config dir");
    std::fs::create_dir_all(dir.join("run")).expect("runtime dir");
}

/// Run `phux ARGS` in `dir` with `stdin` piped in and `extra` environment.
/// The working directory is the test's own, so a relative path an argument
/// names can never land in the source tree.
fn phux_with(dir: &Path, args: &[&str], stdin: &[u8], extra: &[(&str, &Path)]) -> Output {
    let mut child = common::phux_cmd(PHUX)
        .envs(hermetic_env(dir))
        .envs(extra.iter().copied())
        .current_dir(dir)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn phux");
    // A refused invocation may exit before reading; a broken pipe is fine.
    let _ = child.stdin.take().expect("stdin").write_all(stdin);
    child.wait_with_output().expect("wait for phux")
}

fn phux(dir: &Path, args: &[&str]) -> Output {
    phux_with(dir, args, b"", &[])
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn json_doc(out: &Output) -> serde_json::Value {
    let parsed: Result<serde_json::Value, _> = serde_json::from_slice(&out.stdout);
    assert!(
        parsed.is_ok(),
        "expected one JSON document: {}{}",
        text(&out.stdout),
        text(&out.stderr)
    );
    parsed.expect("checked above")
}

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
    let out = phux_with(dir, &args, client.csr_pem.as_bytes(), &[]);
    assert!(out.status.success(), "add-key: {}", text(&out.stderr));
    json_doc(&out)["credential_id"]
        .as_str()
        .expect("credential_id")
        .to_owned()
}

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
            "observe,input@terminal:3",
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
        serde_json::json!(["observe,input@terminal:3", "inventory@global"])
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

/// A UDP port nothing is bound to right now; a collision fails loudly at
/// the readiness wait rather than passing by accident.
fn free_udp_port() -> u16 {
    std::net::UdpSocket::bind("127.0.0.1:0")
        .expect("bind probe socket")
        .local_addr()
        .expect("probe addr")
        .port()
}

fn server_command(dir: &Path, extra: &[&str], env: &[(&str, &str)]) -> std::process::Command {
    let mut command = common::phux_cmd(PHUX);
    command
        .envs(hermetic_env(dir))
        .envs(env.iter().copied())
        .current_dir(dir)
        .arg("server")
        .arg("--socket")
        .arg(dir.join("s.sock"))
        .args(extra)
        .stdin(Stdio::null());
    command
}

fn start_server(dir: &Path, extra: &[&str], env: &[(&str, &str)]) -> Server {
    let child = server_command(dir, extra, env)
        .args(["--exit-after-idle", "120"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn phux server");
    Server(child)
}

fn await_success(dir: &Path, args: &[&str], extra: &[(&str, &Path)]) -> Output {
    let start = Instant::now();
    loop {
        let out = phux_with(dir, args, b"", extra);
        if out.status.success() {
            return out;
        }
        assert!(
            start.elapsed() < READY_DEADLINE,
            "`phux {}` never succeeded: {}",
            args.join(" "),
            text(&out.stderr)
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// An enrolled certificate dials a workload-mTLS QUIC listener; after
/// `revoke`, the next connection with the same certificate is refused by
/// the same running server.
#[test]
#[ignore = "spawns a real server with a workload-mTLS QUIC listener; runs in the e2e lane"]
fn revoke_marks_the_record_and_a_new_connection_is_refused() {
    let dir = TempDir::new().expect("tempdir");
    prepare_dirs(dir.path());
    init_authority(dir.path());
    let client = client_key();
    let cert = dir.path().join("client.pem");
    let key = dir.path().join("client.key");
    std::fs::write(&key, &client.key_pem).expect("write client key");
    std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600)).expect("chmod");
    let id = enroll(dir.path(), &client, &cert, &["*@global"]);

    let port = free_udp_port();
    std::fs::write(
        dir.path().join("config/phux/config.toml"),
        format!("[[remote]]\nname = \"{REMOTE}\"\nendpoint = \"quic://127.0.0.1:{port}\"\n"),
    )
    .expect("write registry");
    let quic = format!("127.0.0.1:{port}");
    let _server = start_server(
        dir.path(),
        &["--quic", &quic],
        &[("PHUX_WORKLOAD_MTLS", "1")],
    );
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

/// Workload mode refuses to start beside a WebTransport listener, which
/// cannot carry a client certificate, and says how to fix it.
#[test]
#[ignore = "runs a real server start; runs in the e2e lane"]
fn workload_mode_refuses_to_start_with_webtransport() {
    let dir = TempDir::new().expect("tempdir");
    prepare_dirs(dir.path());
    init_authority(dir.path());
    let webtransport = format!("127.0.0.1:{}", free_udp_port());
    // A server that wrongly starts exits on its own after the idle window,
    // successfully, which fails the assertion instead of hanging the lane.
    let out = server_command(
        dir.path(),
        &["--webtransport", &webtransport, "--exit-after-idle", "5"],
        &[("PHUX_WORKLOAD_MTLS", "1")],
    )
    .output()
    .expect("run phux server");
    let stderr = text(&out.stderr);
    assert!(!out.status.success(), "workload mode must refuse: {stderr}");
    assert!(
        stderr.contains("PHUX_WORKLOAD_MTLS") && stderr.contains("WebTransport"),
        "the refusal names the setting and the entry point: {stderr}"
    );
}
