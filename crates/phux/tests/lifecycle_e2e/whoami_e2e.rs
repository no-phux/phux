//! `phux whoami` against a real server, end to end (ADR-0106).
//!
//! Two routes. The local socket, where the server reports the kernel's peer
//! uid and no credential. And a `paired` server's loopback QUIC listener
//! forced onto the secure path with `PHUX_WS_SECURE=1`, so the `--remote`
//! dial presents an enrolled certificate and a bearer token minted by `phux
//! pair`, and the server reports the certificate's credential back.
//!
//! Hermetic, like `remote_session_verbs_e2e`: private config, state, and
//! runtime dirs; no ssh and no overlay detection.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]

#[path = "../common/ambient.rs"]
mod common;
#[path = "../common/listeners.rs"]
mod listeners;
#[path = "../common/paired.rs"]
mod paired;

use std::path::Path;
use std::process::{Child, Output, Stdio};
use std::time::{Duration, Instant};

use tempfile::TempDir;

use crate::hermetic::{hermetic_env, prepare_dirs};

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

fn phux(dir: &Path, args: &[&str]) -> Output {
    phux_with(dir, args, b"", &[])
}

fn phux_with(
    dir: &Path,
    args: &[&str],
    stdin: &[u8],
    extra: &[(&str, std::path::PathBuf)],
) -> Output {
    use std::io::Write as _;
    let mut child = common::phux_cmd(crate::runner::phux_bin())
        .envs(hermetic_env(dir))
        .envs(extra.iter().cloned())
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("run phux");
    let _ = child.stdin.take().expect("stdin").write_all(stdin);
    child.wait_with_output().expect("wait for phux")
}

/// `[policy] mode = "paired"` in the hermetic config: the only mode that
/// serves a remote listener (ADR-0179).
fn paired_policy(dir: &Path) {
    paired::write_paired_policy(&dir.join("config/phux/config.toml"));
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
    await_success_with(dir, args, &[])
}

fn await_success_with(dir: &Path, args: &[&str], extra: &[(&str, std::path::PathBuf)]) -> Output {
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

/// The actual pairing CLI uses the provider-neutral override, without
/// claiming those IPs are bound listeners. All server traffic stays loopback.
#[test]
#[ignore = "spawns a real server with a secure listener; runs in the e2e lane"]
fn pairing_uses_explicit_overlay_addresses_without_fabricating_a_route() {
    let dir = TempDir::new().expect("tempdir");
    prepare_dirs(dir.path());
    paired_policy(dir.path());
    let _server = start_server(
        dir.path(),
        &["--quic", listeners::LOOPBACK_ANY_PORT],
        &[("PHUX_WS_SECURE", "1")],
    );
    let socket = dir.path().join("s.sock");
    let quic = listeners::bound_listener_addr(&socket, listeners::RemoteListenerTransport::Quic)
        .to_string();

    for (addresses, expected) in [
        (
            "10.77.0.2,fd77::2,10.77.0.2",
            serde_json::json!(["10.77.0.2", "fd77::2"]),
        ),
        ("", serde_json::json!([])),
        ("10.77.0.2,bad", serde_json::json!([])),
    ] {
        let out = common::phux_cmd(crate::runner::phux_bin())
            .envs(hermetic_env(dir.path()))
            .env("PHUX_OVERLAY_ADDRS", addresses)
            .args([
                "--socket",
                socket.to_str().expect("utf-8 socket"),
                "pair",
                "--json",
            ])
            .stdin(Stdio::null())
            .output()
            .expect("run pairing CLI");
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let paired = json_doc(&out);
        assert_eq!(paired["overlay_addresses"], expected);
        assert_eq!(paired["quic_addr"], quic.as_str());
        assert!(
            paired["connect_link"].is_null(),
            "loopback must not become a remote link"
        );
    }
}

/// A `paired` secure loopback QUIC listener with an enrolled client and a
/// token `phux pair` minted, registered as [`REMOTE`]. Returns the server,
/// the client, the owner socket, and the bound QUIC address.
fn serve_paired_remote(dir: &Path) -> (Server, paired::Enrolled, String, String) {
    prepare_dirs(dir);
    paired_policy(dir);
    let enrolled = paired::enroll_client(dir, |args, stdin| phux_with(dir, args, stdin, &[]));

    // Start the listener first: `phux pair` mints only against a server
    // with a bound remote listener (ADR-0141), and the server re-reads its
    // token store at the next dial. Retried until the server answers; a
    // refused mint writes nothing.
    // A fixed port, as an installed server has: an upgraded image re-binds
    // the configured address rather than inheriting the socket.
    let port = std::net::UdpSocket::bind("127.0.0.1:0")
        .and_then(|probe| probe.local_addr())
        .expect("free udp port")
        .port();
    let server = start_server(
        dir,
        &["--quic", &format!("127.0.0.1:{port}")],
        &[("PHUX_WS_SECURE", "1")],
    );
    let socket = dir.join("s.sock");
    let quic = listeners::bound_listener_addr(&socket, listeners::RemoteListenerTransport::Quic)
        .to_string();
    let socket = socket.to_str().expect("utf-8 socket path").to_owned();
    let paired = json_doc(&await_success(
        dir,
        &["--socket", &socket, "pair", "--json"],
    ));
    assert_eq!(paired["quic_addr"], quic.as_str(), "{paired}");
    let token = paired["token"].as_str().expect("token");
    let token_file = dir.join("loop.token");
    std::fs::write(&token_file, format!("{token}\n")).expect("write token");

    // Appended: an upgraded server re-reads this file and must still find
    // the `paired` mode.
    let config = dir.join("config/phux/config.toml");
    let toml = format!(
        "{}[[remote]]\nname = \"{REMOTE}\"\nendpoint = \"quic://{quic}\"\n\
             token-file = \"{}\"\n",
        std::fs::read_to_string(&config).expect("read config"),
        token_file.display()
    );
    std::fs::write(&config, toml).expect("write registry");
    (server, enrolled, socket, quic)
}

/// Over `--remote`: a `paired` secure loopback QUIC listener admits the dial
/// by an enrolled certificate behind the token `phux pair` minted, and
/// whoami reports the certificate's credential back with no peer uid.
#[test]
#[ignore = "spawns a real server with a QUIC listener; runs in the e2e lane"]
fn whoami_over_remote_reports_the_workload_credential() {
    let dir = TempDir::new().expect("tempdir");
    let (_server, enrolled, _socket, _quic) = serve_paired_remote(dir.path());

    let whoami = ["whoami", "--remote", REMOTE, "--json"];
    let doc = json_doc(&await_success_with(dir.path(), &whoami, &enrolled.env()));
    assert_eq!(doc["auth_route"], "bearer-quic", "{doc}");
    assert_eq!(doc["credential_id"], enrolled.credential_id, "{doc}");
    assert!(doc["principal"].is_string(), "{doc}");
    assert_eq!(
        doc["peer_uid"],
        serde_json::Value::Null,
        "a network route has no kernel peer uid: {doc}"
    );
}

/// A paired client keeps working across a real in-place upgrade: the
/// resumed image inherits the QUIC listener on the same address and admits
/// the same token and certificate (phux-wa4vy: phones came back unpaired).
#[test]
#[ignore = "spawns a real server and re-execs it in place; runs in the e2e lane"]
fn a_paired_remote_client_survives_an_upgrade() {
    let dir = TempDir::new().expect("tempdir");
    let (_server, enrolled, socket, quic) = serve_paired_remote(dir.path());
    let whoami = ["whoami", "--remote", REMOTE, "--json"];
    let before = json_doc(&await_success_with(dir.path(), &whoami, &enrolled.env()));

    let upgrade = phux(dir.path(), &["--socket", &socket, "upgrade"]);
    assert!(
        upgrade.status.success(),
        "upgrade: {}",
        String::from_utf8_lossy(&upgrade.stderr)
    );
    await_success(dir.path(), &["--socket", &socket, "ls"]);
    let after_addr = listeners::bound_listener_addr(
        Path::new(&socket),
        listeners::RemoteListenerTransport::Quic,
    );
    assert_eq!(
        after_addr.to_string(),
        quic,
        "the listener kept its address"
    );

    let after = json_doc(&await_success_with(dir.path(), &whoami, &enrolled.env()));
    assert_eq!(after["credential_id"], before["credential_id"], "{after}");
    assert_eq!(after["auth_route"], "bearer-quic", "{after}");
}
