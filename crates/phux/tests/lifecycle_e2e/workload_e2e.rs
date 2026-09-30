//! `phux workload` (ADR-0116, `docs/spec/workload-auth.md` §8): the
//! authority prints only its fingerprint and no key bytes reach stdout or
//! stderr; a revoked credential's next connection is refused with no restart;
//! workload mode refuses to start beside WebTransport.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]

#[path = "../common/ambient.rs"]
mod common;
#[path = "../common/listeners.rs"]
mod listeners;

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

/// The port the QUIC listener of the server on `socket` bound.
fn bound_quic_port(socket: &Path) -> u16 {
    listeners::bound_listener_addr(socket, listeners::RemoteListenerTransport::Quic).port()
}

/// Register the server on `dir`'s socket as [`REMOTE`], by the QUIC port it
/// bound.
fn register_loopback_remote(dir: &Path) {
    let port = bound_quic_port(&dir.join("s.sock"));
    std::fs::write(
        dir.join("config/phux/config.toml"),
        format!("[[remote]]\nname = \"{REMOTE}\"\nendpoint = \"quic://127.0.0.1:{port}\"\n"),
    )
    .expect("write registry");
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

    let _server = start_server(
        dir.path(),
        &["--quic", listeners::LOOPBACK_ANY_PORT],
        &[("PHUX_WORKLOAD_MTLS", "1")],
    );
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

/// `workload-auth.md` §8/§9 secret sweep across an enrolled lifecycle,
/// revocation included: the private key never reaches argv, environment,
/// stdout, stderr, or the server's trace log.
#[test]
#[ignore = "spawns a real server with a workload-mTLS QUIC listener; runs in the e2e lane"]
fn no_key_nonce_or_signature_bytes_in_argv_env_stdout_stderr_or_trace() {
    let dir = TempDir::new().expect("tempdir");
    prepare_dirs(dir.path());
    init_authority(dir.path());
    let client = client_key();
    let needles = key_needles(&client.key_pem);
    let cert = dir.path().join("client.pem");
    let key = dir.path().join("client.key");
    std::fs::write(&key, &client.key_pem).expect("write client key");
    std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600)).expect("chmod");
    let id = enroll(dir.path(), &client, &cert, &["*@global"]);

    let log = dir.path().join("server.log");
    let stderr = std::fs::File::create(dir.path().join("server.stderr")).expect("stderr file");
    let trace =
        "phux=trace,phux_server=trace,phux_dial=trace,phux_protocol=trace,phux_client=trace,info";
    let server = server_command(
        dir.path(),
        &[
            "--quic",
            listeners::LOOPBACK_ANY_PORT,
            "--exit-after-idle",
            "120",
        ],
        &[
            ("PHUX_WORKLOAD_MTLS", "1"),
            ("RUST_LOG", trace),
            ("PHUX_LOG", log.to_str().expect("utf-8 path")),
        ],
    )
    .stdout(Stdio::null())
    .stderr(stderr)
    .spawn()
    .expect("spawn phux server");
    let server = Server(server);
    register_loopback_remote(dir.path());

    let identity = [
        ("PHUX_WORKLOAD_CERT", cert.as_path()),
        ("PHUX_WORKLOAD_KEY", key.as_path()),
    ];
    let whoami = ["whoami", "--remote", REMOTE, "--json"];
    let mut outputs = vec![await_success(dir.path(), &whoami, &identity)];
    outputs.push(phux(dir.path(), &["workload", "revoke", &id]));
    outputs.push(phux_with(dir.path(), &whoami, b"", &identity));
    outputs.push(phux(dir.path(), &["workload", "list", "--json"]));
    drop(server);

    for out in &outputs {
        assert_no_key_bytes(out, &needles);
    }
    // The key is named by path on every invocation, never carried.
    let passed: Vec<String> = whoami
        .iter()
        .map(|arg| (*arg).to_owned())
        .chain(identity.iter().map(|(_, path)| path.display().to_string()))
        .collect();
    for needle in &needles {
        assert!(
            !passed.iter().any(|value| value.contains(needle.as_str())),
            "key bytes reached argv or the environment"
        );
    }
    let mut traced = std::fs::read_to_string(dir.path().join("server.stderr")).unwrap_or_default();
    traced.push_str(&std::fs::read_to_string(&log).unwrap_or_default());
    assert!(
        traced.contains("TRACE") || traced.contains("DEBUG"),
        "the server's trace log was captured"
    );
    for needle in &needles {
        assert!(
            !traced.contains(needle.as_str()),
            "key bytes reached the server's trace log"
        );
    }
}

/// Workload mode refuses to start beside a WebTransport listener, which
/// cannot carry a client certificate, and says how to fix it.
#[test]
#[ignore = "runs a real server start; runs in the e2e lane"]
fn workload_mode_refuses_to_start_with_webtransport() {
    let dir = TempDir::new().expect("tempdir");
    prepare_dirs(dir.path());
    init_authority(dir.path());
    // A server that wrongly starts exits on its own after the idle window,
    // successfully, which fails the assertion instead of hanging the lane.
    let out = server_command(
        dir.path(),
        &[
            "--webtransport",
            listeners::LOOPBACK_ANY_PORT,
            "--exit-after-idle",
            "5",
        ],
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

/// Poll `child` until it exits or `deadline` passes, then collect it.
fn wait_exit(mut child: Child, deadline: Duration) -> Output {
    let start = Instant::now();
    while child.try_wait().expect("poll child").is_none() {
        if start.elapsed() > deadline {
            let _ = child.kill();
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    child.wait_with_output().expect("collect child")
}

/// A paired server, the owner's socket, and one enrolled workload whose
/// grant holds `?signal` (ADR-0128).
struct HeldWorld {
    dir: TempDir,
    _server: Server,
    cert: PathBuf,
    key: PathBuf,
    credential: String,
}

impl HeldWorld {
    fn start() -> Self {
        let dir = TempDir::new().expect("tempdir");
        prepare_dirs(dir.path());
        init_authority(dir.path());
        let client = client_key();
        let cert = dir.path().join("client.pem");
        let key = dir.path().join("client.key");
        std::fs::write(&key, &client.key_pem).expect("write client key");
        std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600)).expect("chmod");
        let credential = enroll(
            dir.path(),
            &client,
            &cert,
            &["inventory,observe,?signal@global"],
        );
        let server = start_server(
            dir.path(),
            &["--quic", listeners::LOOPBACK_ANY_PORT],
            &[("PHUX_WORKLOAD_MTLS", "1")],
        );
        register_loopback_remote(dir.path());
        let world = Self {
            dir,
            _server: server,
            cert,
            key,
            credential,
        };
        let whoami = ["whoami", "--remote", REMOTE, "--json"];
        await_success(world.dir.path(), &whoami, &world.identity());
        world
    }

    fn identity(&self) -> [(&'static str, &Path); 2] {
        [
            ("PHUX_WORKLOAD_CERT", self.cert.as_path()),
            ("PHUX_WORKLOAD_KEY", self.key.as_path()),
        ]
    }

    fn socket(&self) -> String {
        self.dir.path().join("s.sock").display().to_string()
    }

    /// `phux ARGS --socket S` on the owner's socket, stdin not a terminal.
    fn owner(&self, args: &[&str]) -> Output {
        let socket = self.socket();
        let mut all = args.to_vec();
        all.extend(["--socket", &socket]);
        phux(self.dir.path(), &all)
    }

    /// `phux ARGS` in the background, as the owner or as the workload.
    fn spawn(&self, args: &[&str], as_workload: bool) -> Child {
        let mut command = common::phux_cmd(PHUX);
        command
            .envs(hermetic_env(self.dir.path()))
            .current_dir(self.dir.path())
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if as_workload {
            command.envs(self.identity());
        }
        command.spawn().expect("spawn phux")
    }

    /// The first pending approval, once one exists.
    fn first_approval(&self) -> serde_json::Value {
        let start = Instant::now();
        loop {
            let listed = json_doc(&self.owner(&["approvals", "--json"]));
            if let Some(first) = listed["approvals"].as_array().and_then(|all| all.first()) {
                return first.clone();
            }
            assert!(start.elapsed() < READY_DEADLINE, "nothing was ever held");
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

/// The NDJSON event named `name` in `watched`'s stdout.
fn watched_event(watched: &Output, name: &str) -> serde_json::Value {
    let found = text(&watched.stdout)
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .find(|event| event["event"] == name);
    assert!(
        found.is_some(),
        "`phux watch` rendered no {name}: {}",
        text(&watched.stdout)
    );
    found.expect("checked above")
}

/// ADR-0128 end to end: a `?signal` grant's kill is held; `phux approvals`
/// lists it, `approve` runs it once as the workload, and a resumed `phux watch`
/// renders both approval events. A dangerous verb without `--yes` on a
/// non-terminal exits 2 and sends nothing.
#[test]
#[ignore = "spawns a real server with a workload-mTLS QUIC listener; runs in the e2e lane"]
fn approvals_list_approve_and_watch_render_both_events() {
    let world = HeldWorld::start();
    let created = world.owner(&["new", "-s", "work", "--json"]);
    assert!(created.status.success(), "{}", text(&created.stderr));
    let terminal = json_doc(&created)["terminal_id"]
        .as_u64()
        .expect("terminal_id");
    let pane = format!("@{terminal}");
    let waited = world.owner(&["resource", "wait", &pane, "--timeout", "1", "--json"]);
    let cursor = json_doc(&waited)["cursor"]
        .as_str()
        .expect("a cursor")
        .to_owned();

    // No --yes and no terminal to ask: exit 2, nothing sent.
    let unconfirmed = world.owner(&["kill", &pane]);
    assert_eq!(
        unconfirmed.status.code(),
        Some(2),
        "{}",
        text(&unconfirmed.stderr)
    );
    assert!(text(&unconfirmed.stderr).contains("--yes"));
    assert!(
        world
            .owner(&["resource", "show", &pane, "--json"])
            .status
            .success()
    );

    let socket = world.socket();
    let watch = world.spawn(
        &[
            "watch",
            &pane,
            "--json",
            "--after",
            &cursor,
            "--until",
            "approval_decided",
            "--timeout",
            "60",
            "--socket",
            &socket,
        ],
        false,
    );
    let held_kill = world.spawn(&["kill", "--yes", "--remote", REMOTE, &pane], true);

    let approval = world.first_approval();
    assert_eq!(approval["method"], "KILL_RESOURCE", "{approval}");
    let subject = format!("terminal:{terminal}");
    assert_eq!(approval["subjects"], serde_json::json!([subject]));
    assert_eq!(
        approval["requester"]["credential_id"],
        world.credential.as_str()
    );
    let id = approval["id"].as_str().expect("id").to_owned();
    let still_there = world.owner(&["resource", "show", &pane, "--json"]);
    assert!(still_there.status.success(), "a held kill has not run");

    let approved = world.owner(&["approve", "--yes", &id]);
    assert!(approved.status.success(), "{}", text(&approved.stderr));
    let killed = wait_exit(held_kill, READY_DEADLINE);
    assert!(
        killed.status.success(),
        "the approved kill completes: {}",
        text(&killed.stderr)
    );
    let listed = json_doc(&world.owner(&["approvals", "--json"]));
    assert_eq!(listed["approvals"], serde_json::json!([]));

    let watched = wait_exit(watch, READY_DEADLINE);
    assert!(watched.status.success(), "{}", text(&watched.stderr));
    assert_eq!(
        watched_event(&watched, "approval_requested")["id"],
        id.as_str()
    );
    let decided = watched_event(&watched, "approval_decided");
    assert_eq!(decided["id"], id.as_str());
    assert_eq!(decided["outcome"], "approved");
}

/// The far host of a `phux host add` enrollment: its own state, config, and
/// socket, reached through a fake `ssh` that runs the real binary there.
struct FarHost {
    root: PathBuf,
}

impl FarHost {
    fn new(dir: &Path) -> Self {
        let root = dir.join("far");
        prepare_dirs(&root);
        // `mode = "paired"`: every TLS connection must present a workload
        // certificate the registry admits (workload-auth §8).
        std::fs::write(
            root.join("config/phux/config.toml"),
            "[policy]\nmode = \"paired\"\n",
        )
        .expect("far config");
        Self { root }
    }

    fn env(&self) -> Vec<(&'static str, PathBuf)> {
        let mut env = hermetic_env(&self.root);
        env.push(("HOME", self.root.clone()));
        env.push(("PHUX_SOCKET", self.root.join("s.sock")));
        env
    }

    /// `phux ARGS` on the far host.
    fn phux(&self, args: &[&str]) -> Output {
        common::phux_cmd(PHUX)
            .envs(self.env())
            .current_dir(&self.root)
            .args(args)
            .stdin(Stdio::null())
            .output()
            .expect("run far phux")
    }

    /// The far host's server: paired policy, one QUIC listener, returned once
    /// it listens. Its log goes to `server.stderr` under the far root, for
    /// [`Self::server_log`].
    fn serve(&self, quic: &str) -> Server {
        let stderr = std::fs::File::create(self.root.join("server.stderr")).expect("stderr file");
        let child = common::phux_cmd(PHUX)
            .envs(self.env())
            .env("RUST_LOG", "info")
            .current_dir(&self.root)
            .args(["server", "--quic", quic, "--exit-after-idle", "120"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(stderr)
            .spawn()
            .expect("spawn far server");
        let server = Server(child);
        let start = Instant::now();
        while !self.server_log().contains("QUIC listening") {
            assert!(
                start.elapsed() < READY_DEADLINE,
                "the far server never listened: {}",
                self.server_log()
            );
            std::thread::sleep(Duration::from_millis(100));
        }
        server
    }

    /// A fake `ssh` that logs its argv to `ssh-calls` and its stdin to
    /// `ssh-stdin`, and runs `phux` on this far host with that stdin:
    /// `ssh -G -- HOST` names loopback, and `ssh -o BatchMode=yes HOST phux
    /// ARGS...` runs the real binary. While a `tamper-add-key` file exists
    /// beside it, `workload add-key` still enrolls on the far host but its
    /// reply arrives with the leaf relabelled as a private key, which the
    /// client must refuse after the far host committed.
    fn fake_ssh(&self, dir: &Path) -> PathBuf {
        use std::fmt::Write as _;
        let exports = self
            .env()
            .iter()
            .fold(String::new(), |mut exports, (key, value)| {
                let _ = writeln!(exports, "export {key}='{}'", value.display());
                exports
            });
        let script = format!(
            "#!/bin/sh\n\
             printf '%s\\n' \"$*\" >> '{calls}'\n\
             if [ \"$1\" = \"-G\" ]; then echo 'hostname 127.0.0.1'; exit 0; fi\n\
             shift 4\n\
             {exports}\
             case \"$*\" in *'workload add-key'*) if [ -f '{tamper}' ]; then \
               tee -a '{stdin}' | '{PHUX}' \"$@\" | sed 's/-----BEGIN CERTIFICATE-----/-----BEGIN PRIVATE KEY-----/'; exit 0; fi ;; esac\n\
             tee -a '{stdin}' | '{PHUX}' \"$@\"\n",
            calls = dir.join("ssh-calls").display(),
            stdin = dir.join("ssh-stdin").display(),
            tamper = dir.join("tamper-add-key").display(),
        );
        let path = dir.join("fake-ssh");
        std::fs::write(&path, script).expect("write fake ssh");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        path
    }

    /// What the far server logged so far, for a failure message.
    fn server_log(&self) -> String {
        std::fs::read_to_string(self.root.join("server.stderr")).unwrap_or_default()
    }

    fn credentials(&self) -> Vec<serde_json::Value> {
        json_doc(&self.phux(&["workload", "list", "--json"]))["credentials"]
            .as_array()
            .expect("credentials")
            .clone()
    }
}

/// `phux ARGS` on the enrolling client: its own sandbox, `$PHUX_SSH` at the
/// fake, and no `PHUX_WORKLOAD_*` identity in the environment.
fn client_phux(dir: &Path, ssh: &Path, args: &[&str]) -> Output {
    let client = dir.join("client");
    common::phux_cmd(PHUX)
        .envs(hermetic_env(&client))
        .env("HOME", &client)
        .env("PHUX_SSH", ssh)
        .current_dir(&client)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("run client phux")
}

/// The `[[remote]]` entry `host add` wrote for `name`.
fn remote_entry(dir: &Path, name: &str) -> toml::Value {
    let raw =
        std::fs::read_to_string(dir.join("client/config/phux/config.toml")).expect("client config");
    let config: toml::Value = toml::from_str(&raw).expect("client config parses");
    config["remote"]
        .as_array()
        .expect("remotes")
        .iter()
        .find(|entry| entry["name"].as_str() == Some(name))
        .expect("the remote entry")
        .clone()
}

fn entry_path(entry: &toml::Value, key: &str) -> PathBuf {
    PathBuf::from(
        entry
            .get(key)
            .and_then(toml::Value::as_str)
            .expect("a path in the remote entry"),
    )
}

/// One raw QUIC connection to the far listener, dialed the way the registry
/// entry says (its token, pin, and enrolled certificate), with HELLO sent.
struct RawSession {
    runtime: tokio::runtime::Runtime,
    connection: phux_dial::quic::QuicConnection,
}

impl RawSession {
    fn open(entry: &toml::Value, port: u16) -> Self {
        use phux_protocol::wire::frame::FrameKind;

        let token = std::fs::read_to_string(entry_path(entry, "token-file")).expect("token");
        let dial = phux_dial::quic::QuicDial {
            addr: std::net::SocketAddr::from(([127, 0, 0, 1], port)),
            server_name: "localhost".to_owned(),
            token: Some(phux_dial::quic::parse_token_hex(token.trim()).expect("hex token")),
            trust: phux_dial::CertTrust::Pinned(
                entry["cert-fingerprint"].as_str().expect("pin").to_owned(),
            ),
            identity: Some(phux_dial::TlsClientIdentity::PemFiles {
                certificate: entry_path(entry, "client-cert"),
                private_key: entry_path(entry, "client-key"),
            }),
        };
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let mut connection = runtime
            .block_on(phux_dial::quic::dial(&dial))
            .expect("the TLS handshake completes on the client side");
        let hello = FrameKind::Hello {
            client_name: "enrollment-e2e".to_owned(),
            protocol_major: phux_protocol::PROTOCOL_VERSION.major,
            protocol_minor: phux_protocol::PROTOCOL_VERSION.minor,
            protocol_patch: phux_protocol::PROTOCOL_VERSION.patch,
            client_caps: phux_protocol::caps::ClientCapabilities::default(),
        };
        let mut encoded = bytes::BytesMut::new();
        hello.encode(&mut encoded);
        // A refused connection may already be closing; the reads say so.
        let _ = runtime.block_on(connection.2.write_all(&encoded));
        Self {
            runtime,
            connection,
        }
    }

    /// The next frame, or `None` once the server closed the stream.
    fn next_frame(&mut self) -> Option<phux_protocol::wire::frame::FrameKind> {
        let recv = &mut self.connection.3;
        self.runtime.block_on(async {
            let mut header = [0_u8; 4];
            let read = tokio::time::timeout(READY_DEADLINE, recv.read_exact(&mut header)).await;
            let Ok(Ok(())) = read else { return None };
            let len = u32::from_be_bytes(header) as usize;
            let mut packet = header.to_vec();
            packet.resize(4 + len, 0);
            recv.read_exact(&mut packet[4..]).await.ok()?;
            let (frame, rest) =
                phux_protocol::wire::frame::FrameKind::decode(&packet).expect("decode frame");
            assert!(rest.is_empty());
            Some(frame)
        })
    }

    /// Every frame until the server closes.
    fn frames_until_close(&mut self) -> Vec<phux_protocol::wire::frame::FrameKind> {
        std::iter::from_fn(|| self.next_frame()).collect()
    }

    /// Read until `HELLO_OK`, failing if the server closes first.
    fn await_hello_ok(&mut self) {
        use phux_protocol::wire::frame::FrameKind;
        let mut seen = Vec::new();
        let admitted = std::iter::from_fn(|| self.next_frame())
            .inspect(|frame| seen.push(frame.clone()))
            .any(|frame| matches!(frame, FrameKind::HelloOk { .. }));
        assert!(
            admitted,
            "the enrolled certificate was not admitted: {seen:?}"
        );
    }

    /// Why the connection closed, as quinn reports it.
    fn close_reason(&self) -> String {
        format!("{:?}", self.connection.1.close_reason())
    }
}

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

/// Swap the entry's identity for one the far host issued for `days`: a
/// client key and CSR made here, signed by `add-key --expires-in`, stored
/// under the names `host add` gives an enrolled pair. Returns its id.
fn install_short_lived_identity(host: &Enrolled, days: i64) -> String {
    let client = client_key();
    let chain = host.far.root.join("short.pem");
    let seconds = (days * 86_400).to_string();
    let mut far = common::phux_cmd(PHUX)
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
