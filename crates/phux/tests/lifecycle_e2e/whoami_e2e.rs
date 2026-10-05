//! `phux whoami` against a real server, end to end (ADR-0106).
//!
//! Two routes. The local socket, where the server reports the kernel's peer
//! uid and no credential. And a loopback QUIC listener forced onto the secure
//! path with `PHUX_WS_SECURE=1`, so the `--remote` dial must present a bearer
//! token minted by `phux pair` and the server reports that credential back.
//!
//! Hermetic, like `remote_session_verbs_e2e`: private config, state, and
//! runtime dirs; no ssh and no overlay detection.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]

#[path = "../common/ambient.rs"]
mod common;
#[path = "../common/listeners.rs"]
mod listeners;

use std::path::{Path, PathBuf};
use std::process::{Child, Output, Stdio};
use std::time::{Duration, Instant};

use tempfile::TempDir;

/// The registry name the loopback listener is registered under.
const REMOTE: &str = "loop";

/// How long a freshly started server has to answer its first `whoami`.
const READY_DEADLINE: Duration = Duration::from_secs(30);

/// A running `phux server`, killed on drop.
struct Server(Child);

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// The environment every process in this test runs under.
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

fn phux(dir: &Path, args: &[&str]) -> Output {
    common::phux_cmd(crate::runner::phux_bin())
        .envs(hermetic_env(dir))
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("run phux")
}

/// Start `phux server --socket DIR/s.sock EXTRA...` under the hermetic env.
fn start_server(dir: &Path, extra: &[&str], env: &[(&str, &str)]) -> Server {
    let child = common::phux_cmd(crate::runner::phux_bin())
        .envs(hermetic_env(dir))
        .envs(env.iter().copied())
        .arg("server")
        .arg("--socket")
        .arg(dir.join("s.sock"))
        .args(extra)
        // Backstop only: the Drop kills the server, but a panic that skips it
        // must not leave a daemon behind for long.
        .args(["--exit-after-idle", "120"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn phux server");
    Server(child)
}

/// Run `phux ARGS` until it succeeds, failing after [`READY_DEADLINE`].
fn await_success(dir: &Path, args: &[&str]) -> Output {
    let start = Instant::now();
    loop {
        let out = phux(dir, args);
        if out.status.success() {
            return out;
        }
        assert!(
            start.elapsed() < READY_DEADLINE,
            "`phux {}` never succeeded: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr)
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn json_doc(out: &Output) -> serde_json::Value {
    serde_json::from_slice(&out.stdout).expect("whoami --json is one JSON document")
}

/// Over the local socket: the kernel peer uid, no credential, and the same
/// user on both ends. The prose view prints one labelled field per line.
#[test]
#[ignore = "spawns a real server; runs in the e2e lane"]
fn whoami_reports_the_local_socket_peer() {
    let dir = TempDir::new().expect("tempdir");
    prepare_dirs(dir.path());
    let socket = dir.path().join("s.sock");
    let socket = socket.to_str().expect("utf-8 path");
    let _server = start_server(dir.path(), &[], &[]);

    let doc = json_doc(&await_success(
        dir.path(),
        &["--socket", socket, "whoami", "--json"],
    ));
    assert_eq!(doc["schema_version"], 1, "{doc}");
    assert_eq!(doc["auth_route"], "uds", "{doc}");
    assert_eq!(doc["principal"], serde_json::Value::Null, "{doc}");
    assert_eq!(doc["credential_id"], serde_json::Value::Null, "{doc}");
    assert!(doc["peer_uid"].is_u64(), "{doc}");
    assert_eq!(
        doc["peer_uid"], doc["serving_user"]["uid"],
        "a local client and its server are the same user: {doc}"
    );
    assert!(doc["server_version"].is_string(), "{doc}");
    assert_eq!(
        doc["ssh_client"],
        serde_json::Value::Null,
        "a direct local client announced no ssh origin: {doc}"
    );

    let prose = phux(dir.path(), &["--socket", socket, "whoami"]);
    assert!(prose.status.success());
    let text = String::from_utf8_lossy(&prose.stdout);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 8, "{text}");
    assert!(lines.contains(&"auth_route:     uds"), "{text}");
    assert!(lines.contains(&"principal:      none"), "{text}");
    assert!(lines.contains(&"ssh_client:     none"), "{text}");
}

/// Over `--remote`: a secure loopback QUIC listener admits the dial by the
/// token `phux pair` minted, and whoami reports that credential back with
/// no peer uid.
#[test]
#[ignore = "spawns a real server with a QUIC listener; runs in the e2e lane"]
fn whoami_over_remote_reports_the_bearer_credential() {
    let dir = TempDir::new().expect("tempdir");
    prepare_dirs(dir.path());

    // Start the listener first: `phux pair` mints only against a server
    // with a bound remote listener (ADR-0141), and the server re-reads its
    // token store at the next dial. Retried until the server answers; a
    // refused mint writes nothing.
    let _server = start_server(
        dir.path(),
        &["--quic", listeners::LOOPBACK_ANY_PORT],
        &[("PHUX_WS_SECURE", "1")],
    );
    let socket = dir.path().join("s.sock");
    let quic = listeners::bound_listener_addr(&socket, listeners::RemoteListenerTransport::Quic)
        .to_string();
    let socket = socket.to_str().expect("utf-8 socket path");
    let paired = json_doc(&await_success(
        dir.path(),
        &["--socket", socket, "pair", "--json"],
    ));
    assert_eq!(paired["quic_addr"], quic.as_str(), "{paired}");
    let token = paired["token"].as_str().expect("token");
    let credential_id = paired["credential_id"].as_str().expect("credential_id");
    let token_file = dir.path().join("loop.token");
    std::fs::write(&token_file, format!("{token}\n")).expect("write token");

    std::fs::write(
        dir.path().join("config/phux/config.toml"),
        format!(
            "[[remote]]\nname = \"{REMOTE}\"\nendpoint = \"quic://{quic}\"\n\
             token-file = \"{}\"\n",
            token_file.display()
        ),
    )
    .expect("write registry");

    let doc = json_doc(&await_success(
        dir.path(),
        &["whoami", "--remote", REMOTE, "--json"],
    ));
    assert_eq!(doc["auth_route"], "bearer-quic", "{doc}");
    assert_eq!(doc["credential_id"], credential_id, "{doc}");
    assert!(doc["principal"].is_string(), "{doc}");
    assert_eq!(
        doc["peer_uid"],
        serde_json::Value::Null,
        "a network route has no kernel peer uid: {doc}"
    );
}
