//! `phux whoami` against a real server, end to end (ADR-0106).
//!
//! Two routes. The local socket, where the server reports the kernel's peer
//! uid and no credential. And a `paired` server's loopback QUIC listener
//! forced onto the secure path with `PHUX_WS_SECURE=1`, so the `--remote`
//! dial presents an enrolled certificate and a bearer token minted by `phux
//! pair`, and the server reports the certificate's credential back.
//! `phux doctor --remote` rides the same paired dial.
//!
//! Hermetic, like `remote_session_verbs_e2e`: private config, state, and
//! runtime dirs; no ssh and no overlay detection.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]

use crate::{common, listeners, paired};

use std::path::Path;
use std::process::Stdio;

use tempfile::TempDir;

use crate::hermetic::{
    NO_ENV, Server, await_success, hermetic_env, json_doc, phux, phux_with, prepare_dirs,
    start_server,
};

/// The registry name the loopback listener is registered under.
const REMOTE: &str = "loop";

/// `[policy] mode = "paired"` in the hermetic config: the only mode that
/// serves a remote listener (ADR-0179).
fn paired_policy(dir: &Path) {
    paired::write_paired_policy(&dir.join("config/phux/config.toml"));
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
        NO_ENV,
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
    let enrolled = paired::enroll_client(dir, |args, stdin| phux_with(dir, args, stdin, NO_ENV));

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
        NO_ENV,
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
    let doc = json_doc(&await_success(dir.path(), &whoami, &enrolled.env()));
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
    let before = json_doc(&await_success(dir.path(), &whoami, &enrolled.env()));

    let upgrade = phux(dir.path(), &["--socket", &socket, "upgrade"]);
    assert!(
        upgrade.status.success(),
        "upgrade: {}",
        String::from_utf8_lossy(&upgrade.stderr)
    );
    await_success(dir.path(), &["--socket", &socket, "ls"], NO_ENV);
    let after_addr = listeners::bound_listener_addr(
        Path::new(&socket),
        listeners::RemoteListenerTransport::Quic,
    );
    assert_eq!(
        after_addr.to_string(),
        quic,
        "the listener kept its address"
    );

    let after = json_doc(&await_success(dir.path(), &whoami, &enrolled.env()));
    assert_eq!(after["credential_id"], before["credential_id"], "{after}");
    assert_eq!(after["auth_route"], "bearer-quic", "{after}");
}

/// `phux doctor --remote` reads the same list over a real paired QUIC dial,
/// and shows every fix as a command run from here.
#[test]
#[ignore = "spawns a real server with a QUIC listener; runs in the e2e lane"]
fn doctor_remote_shows_the_hosts_checklist() {
    let dir = TempDir::new().expect("tempdir");
    let (_server, enrolled, socket, _quic) = serve_paired_remote(dir.path());
    // Exits 1 when a local check fails; the document is whole either way.
    let local = json_doc(&phux(
        dir.path(),
        &["--socket", &socket, "doctor", "--json"],
    ));
    let doc = json_doc(&await_success(
        dir.path(),
        &["doctor", "--remote", REMOTE, "--json"],
        &enrolled.env(),
    ));
    assert_eq!(doc["host"], REMOTE, "{doc}");
    let ids = |doc: &serde_json::Value| {
        doc["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| item["id"].clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(ids(&doc["checklist"]), ids(&local["checklist"]), "{doc}");

    let prose = await_success(dir.path(), &["doctor", "--remote", REMOTE], &enrolled.env());
    let text = String::from_utf8_lossy(&prose.stdout);
    assert!(text.contains(&format!("What works on {REMOTE}")), "{text}");
    for fix in text
        .lines()
        .filter_map(|line| line.trim().strip_prefix("fix: "))
    {
        assert!(
            fix.starts_with(&format!("phux new --remote '{REMOTE}' -- sh -lc ")),
            "{text}"
        );
    }
}
